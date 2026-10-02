//! A deterministic interrupted HTTP body for reproducing direct-download retries.
use axum::http::Method;

use crate::*;

const TOTAL_BYTES: u64 = 8 * 1024 * 1024;
const FAIL_AFTER_BYTES: u64 = 2 * 1024 * 1024;
const CHUNK_BYTES: u64 = 64 * 1024;

#[derive(Default)]
pub(crate) struct RetryCase {
    requests: u64,
    failure_injected: bool,
}

pub(crate) async fn index(headers: HeaderMap) -> Html<String> {
    let url = format!(
        "{}/mp4/retry-test/{}/video.mp4",
        request_base_url(&headers),
        Uuid::new_v4()
    );
    Html(format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
        <title>MP4 自动重试复现</title>
        <style>body{{font-family:system-ui;max-width:900px;margin:40px auto;padding:20px;line-height:1.8}}
        input{{width:100%;box-sizing:border-box;padding:12px}}code{{background:#eee;padding:3px}}</style></head>
        <body><h1>MP4 自动重试复现</h1>
        <p>使用 8 MiB 固定模拟数据，首次无 Range 的 GET 请求发送 2 MiB 后主动断开，后续请求正常返回。</p>
        <p>这是下载测试数据，不是可播放的视频。无需选择本地文件或安装 FFmpeg。</p>
        <input aria-label="测试下载地址" readonly value="{}" onclick="this.select()">
        <ol><li>复制上面的地址，在应用中新建下载任务，名称填写 retry-test.mp4，保存到空目录。</li>
        <li>等待自动重试，不要点击暂停、继续或打开播放器，也不要先在浏览器中访问此下载地址。</li>
        <li>当前缺陷预期留下 retry-test.mp4.partial，并完成 retry-test (1).mp4。</li>
        <li>查看服务终端：有缺陷时重试没有 Range；正确续传应带有 Range: bytes=2097152-。</li></ol>
        <p>HEAD 不消耗故障机会；合法 Range 返回 206。刷新此页生成新用例地址，重复测试时请使用新的空目录。</p>
        <p><a href="/mp4/retry-test">生成新用例</a> · <a href="/mp4">返回 MP4 页面</a></p></body></html>"#,
        escape_html(&url)
    ))
}

pub(crate) async fn serve(
    State(state): State<AppState>,
    AxumPath(case_id): AxumPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if case_id.is_empty()
        || case_id.len() > 64
        || !case_id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(AppError::bad_request("用例 ID 必须为 1–64 个字母、数字、短横线或下划线"));
    }

    // HEAD probes neither request a body nor consume the injected failure.
    let range_header = if method == Method::HEAD { None } else { headers.get(header::RANGE) };
    let range = match range_header {
        Some(value) => match value.to_str().ok().and_then(|v| parse_single_byte_range(v, TOTAL_BYTES)) {
            Some(range) => range,
            None => return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{TOTAL_BYTES}"))],
            ).into_response()),
        },
        None => (StatusCode::OK, 0, TOTAL_BYTES - 1),
    };
    let (status, start, end) = range;
    let content_length = end - start + 1;
    let (request_number, inject_failure) = if method == Method::HEAD {
        (0, false)
    } else {
        let mut cases = state.mp4_retry_cases.lock().await;
        let case = cases.entry(case_id.clone()).or_default();
        case.requests += 1;
        let inject = range_header.is_none() && !case.failure_injected;
        case.failure_injected |= inject;
        (case.requests, inject)
    };

    println!(
        "[mp4-retry {case_id} #{request_number}] {method} Range={} -> {} start={start} Content-Length={content_length} inject_failure={inject_failure}",
        range_header.and_then(|value| value.to_str().ok()).unwrap_or("<none>"),
        status.as_u16(),
    );
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response_headers.insert(header::CONTENT_LENGTH, HeaderValue::from(content_length));
    if status == StatusCode::PARTIAL_CONTENT {
        response_headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{TOTAL_BYTES}")).unwrap(),
        );
    }
    if method == Method::HEAD {
        return Ok((status, response_headers, Body::empty()).into_response());
    }

    let stream = async_stream::stream! {
        let send_length = if inject_failure { FAIL_AFTER_BYTES } else { content_length };
        let mut sent = 0u64;
        while sent < send_length {
            let length = CHUNK_BYTES.min(send_length - sent);
            // Offset-dependent content lets a client verify the resumed file byte-for-byte.
            let data: Vec<u8> = (start + sent..start + sent + length)
                .map(|offset| (offset % 251) as u8)
                .collect();
            sent += length;
            // Hyper may stop polling as soon as Content-Length is satisfied.
            if !inject_failure && sent == send_length {
                println!("[mp4-retry {case_id} #{request_number}] 响应体生成完成：{sent} 字节");
            }
            yield Ok::<Bytes, std::io::Error>(Bytes::from(data));
            // Pace the transfer and let Hyper flush bytes before the deliberate body error.
            tokio::time::sleep(Duration::from_millis(64)).await;
        }
        if inject_failure {
            println!("[mp4-retry {case_id} #{request_number}] 故意中断：已发送 {sent}/{content_length} 字节");
            yield Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "injected MP4 retry failure"));
        }
    };
    Ok((status, response_headers, Body::from_stream(stream)).into_response())
}
