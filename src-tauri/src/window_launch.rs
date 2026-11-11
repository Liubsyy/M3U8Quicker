pub(crate) fn is_preview_launch<'a>(mut args: impl Iterator<Item = &'a str>) -> bool {
    // 与 deep-link 插件的 CLI 规则保持一致：可执行文件后只能有一个链接参数。
    args.next();
    let Some(link) = args.next() else {
        return false;
    };
    if args.next().is_some() {
        return false;
    }
    url::Url::parse(link).is_ok_and(|url| is_preview_url(&url))
}

fn is_preview_url(url: &url::Url) -> bool {
    let action = url.host_str().filter(|host| !host.is_empty())
        .unwrap_or_else(|| url.path().trim_start_matches('/'));
    url.scheme() == "m3u8quicker"
        && action.eq_ignore_ascii_case("preview")
        && url.query_pairs().find(|(key, _)| key == "url")
            .is_some_and(|(_, value)| !value.trim().is_empty())
}

// macOS 的链接通过 Opened 事件传递，不能用进程参数判断冷启动原因。
// 在本轮系统事件处理完后决定是否显示主窗口，避免 Ready / Reopen 抢先显示。
#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
pub(crate) struct MacWindowActivation {
    startup_finished: bool,
    reopen_requested: bool,
    preview_only: Option<bool>,
}

#[cfg(any(target_os = "macos", test))]
impl MacWindowActivation {
    pub(crate) fn opened(&mut self, urls: &[url::Url]) {
        let preview_only = !urls.is_empty() && urls.iter().all(is_preview_url);
        self.preview_only = Some(self.preview_only.unwrap_or(true) && preview_only);
    }

    pub(crate) fn reopen(&mut self, has_visible_windows: bool) {
        self.reopen_requested |= !has_visible_windows;
    }

    pub(crate) fn finish_event_batch(&mut self) -> bool {
        let show_main = match self.preview_only.take() {
            Some(preview_only) => !preview_only,
            None => !self.startup_finished || self.reopen_requested,
        };
        self.startup_finished = true;
        self.reopen_requested = false;
        show_main
    }
}

#[cfg(test)]
mod tests {
    use super::{is_preview_launch, MacWindowActivation};

    fn preview_url() -> url::Url {
        url::Url::parse("m3u8quicker://preview?url=https://example.com/video.m3u8").unwrap()
    }

    #[test]
    fn preview_links_keep_main_window_hidden() {
        for link in [
            "m3u8quicker://preview?url=https%3A%2F%2Fexample.com%2Fvideo.m3u8",
            "m3u8quicker://PREVIEW?url=https%3A%2F%2Fexample.com%2Fvideo.mp4&title=Example",
            "m3u8quicker:///preview?url=%7B%22video%22%3A%5B%5D%7D",
        ] {
            assert!(is_preview_launch(["app.exe", link].into_iter()), "{link}");
        }
    }

    #[test]
    fn other_launches_show_main_window() {
        assert!(!is_preview_launch(["app.exe"].into_iter()));
        for link in [
            "m3u8quicker://new-task?url=https://example.com/video.m3u8",
            "m3u8quicker://batch-download?url=https://example.com/video.m3u8",
            "m3u8quicker://new-live-record?url=https://example.com/live.m3u8",
            "https://preview?url=https://example.com/video.m3u8",
            "m3u8quicker://preview",
            "m3u8quicker://preview?url=%20%09",
            "m3u8quicker://preview?url=&url=https://example.com/video.m3u8",
            "not a url",
        ] {
            assert!(!is_preview_launch(["app.exe", link].into_iter()), "{link}");
        }
        assert!(!is_preview_launch([
            "app.exe", "m3u8quicker://preview?url=https://example.com/video.m3u8", "extra",
        ].into_iter()));
    }

    #[test]
    fn mac_preview_cold_start_stays_hidden() {
        let mut activation = MacWindowActivation::default();
        activation.opened(&[preview_url()]);
        assert!(!activation.finish_event_batch());
        assert!(!activation.finish_event_batch());
    }

    #[test]
    fn mac_normal_start_and_later_dock_click_show_main() {
        let mut activation = MacWindowActivation::default();
        assert!(activation.finish_event_batch());
        assert!(!activation.finish_event_batch());
        activation.opened(&[preview_url()]);
        assert!(!activation.finish_event_batch());
        activation.reopen(false);
        assert!(activation.finish_event_batch());
        assert!(!activation.finish_event_batch());
        activation.reopen(true);
        assert!(!activation.finish_event_batch());
    }

    #[test]
    fn mac_preview_suppresses_reopen_in_either_event_order() {
        for cold_start in [true, false] {
            for reopen_first in [true, false] {
                let mut activation = MacWindowActivation::default();
                if !cold_start {
                    assert!(activation.finish_event_batch());
                }
                if reopen_first {
                    activation.reopen(false);
                    activation.opened(&[preview_url()]);
                } else {
                    activation.opened(&[preview_url()]);
                    activation.reopen(false);
                }
                assert!(!activation.finish_event_batch());
                // 预览结束后，后续独立的 Dock 点击仍可正常打开主窗口。
                activation.reopen(false);
                assert!(activation.finish_event_batch());
            }
        }
    }

    #[test]
    fn mac_other_links_still_show_main() {
        let download = url::Url::parse("m3u8quicker://new-task?url=https://example.com/video.m3u8").unwrap();
        for urls in [vec![download.clone()], vec![preview_url(), download.clone()], vec![]] {
            let mut activation = MacWindowActivation::default();
            activation.opened(&urls);
            assert!(activation.finish_event_batch());
        }
        // 同一轮收到多个 Opened 事件时，下载请求不能被后来的预览请求覆盖。
        let mut activation = MacWindowActivation::default();
        activation.opened(&[download]);
        activation.opened(&[preview_url()]);
        assert!(activation.finish_event_batch());
    }
}
