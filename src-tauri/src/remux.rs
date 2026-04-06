use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use bytes::Bytes;
use h264_reader::nal::sps::SeqParameterSet;
use h264_reader::nal::{Nal, RefNal, UnitType};
use mp4::{
    AacConfig, AudioObjectType, AvcConfig, ChannelConfig, MediaConfig, Mp4Config, Mp4Sample,
    Mp4Writer, SampleFreqIndex, TrackConfig, TrackType,
};

use crate::error::AppError;

const TS_PACKET_SIZE: usize = 188;
const MPEG_TS_TIMESCALE: u32 = 90_000;
const AAC_SAMPLES_PER_FRAME: u32 = 1_024;
const LAST_VIDEO_SAMPLE_DURATION_FALLBACK: u32 = 3_000;
const PTS_WRAP_THRESHOLD: u64 = 1 << 32;
const PTS_FULL_CYCLE: u64 = 1 << 33;

#[derive(Default)]
struct StreamMap {
    pmt_pid: Option<u16>,
    video_pid: Option<u16>,
    audio_pid: Option<u16>,
    unsupported_video_stream_type: Option<u8>,
    unsupported_audio_stream_type: Option<u8>,
}

struct PesAssembler {
    pts: Option<u64>,
    dts: Option<u64>,
    data: Vec<u8>,
}

struct PesPacket {
    pts: Option<u64>,
    dts: Option<u64>,
    data: Vec<u8>,
}

#[derive(Default)]
struct TimestampUnroller {
    last_raw: Option<u64>,
    wrap_base: u64,
}

impl TimestampUnroller {
    fn unroll(&mut self, value: u64) -> u64 {
        if let Some(last_raw) = self.last_raw {
            if value < last_raw && last_raw - value > PTS_WRAP_THRESHOLD {
                self.wrap_base += PTS_FULL_CYCLE;
            }
        }

        self.last_raw = Some(value);
        self.wrap_base + value
    }
}

#[derive(Default)]
struct StreamTimestampState {
    pts: TimestampUnroller,
    dts: TimestampUnroller,
}

struct VideoTrack {
    config: TrackConfig,
    samples: Vec<Mp4Sample>,
}

struct AudioTrack {
    config: TrackConfig,
    samples: Vec<Mp4Sample>,
}

struct RawVideoSample {
    pts: Option<u64>,
    dts: Option<u64>,
    is_sync: bool,
    bytes: Vec<u8>,
}

struct AudioFrameHeader {
    header_len: usize,
    frame_len: usize,
    profile: AudioObjectType,
    freq_index: SampleFreqIndex,
    channel_config: ChannelConfig,
    sample_rate: u32,
}

#[derive(Clone, Copy)]
struct AudioTrackInfo {
    profile: AudioObjectType,
    freq_index: SampleFreqIndex,
    channel_config: ChannelConfig,
    sample_rate: u32,
}

pub fn remux_ts_to_mp4_file(ts_path: &Path, mp4_path: &Path) -> Result<(), AppError> {
    let ts_data = std::fs::read(ts_path)?;
    let (video_track, audio_track) = parse_transport_stream(&ts_data)?;

    if video_track.is_none() && audio_track.is_none() {
        return Err(AppError::Conversion(
            "TS 文件中未找到可转换的 H.264/AAC 轨道".to_string(),
        ));
    }

    let movie_timescale = compute_movie_timescale(video_track.as_ref(), audio_track.as_ref())?;

    let mut compatible_brands = vec!["isom".parse().unwrap(), "iso2".parse().unwrap()];
    if video_track.is_some() {
        compatible_brands.push("avc1".parse().unwrap());
    }
    compatible_brands.push("mp41".parse().unwrap());

    let config = Mp4Config {
        major_brand: "isom".parse().unwrap(),
        minor_version: 512,
        compatible_brands,
        timescale: movie_timescale,
    };

    let output = File::options()
        .write(true)
        .create_new(true)
        .open(mp4_path)?;
    let mut writer = Mp4Writer::write_start(BufWriter::new(output), &config)
        .map_err(|error| AppError::Conversion(format!("MP4 初始化失败: {}", error)))?;

    let mut next_track_id = 1u32;

    if let Some(video_track) = video_track {
        writer
            .add_track(&video_track.config)
            .map_err(|error| AppError::Conversion(format!("视频轨写入失败: {}", error)))?;
        for sample in &video_track.samples {
            writer
                .write_sample(next_track_id, sample)
                .map_err(|error| AppError::Conversion(format!("视频样本写入失败: {}", error)))?;
        }
        next_track_id += 1;
    }

    if let Some(audio_track) = audio_track {
        writer
            .add_track(&audio_track.config)
            .map_err(|error| AppError::Conversion(format!("音频轨写入失败: {}", error)))?;
        for sample in &audio_track.samples {
            writer
                .write_sample(next_track_id, sample)
                .map_err(|error| AppError::Conversion(format!("音频样本写入失败: {}", error)))?;
        }
    }

    writer
        .write_end()
        .map_err(|error| AppError::Conversion(format!("MP4 收尾失败: {}", error)))?;
    writer
        .into_writer()
        .flush()
        .map_err(|error| AppError::Conversion(format!("MP4 输出失败: {}", error)))?;

    Ok(())
}

fn compute_movie_timescale(
    video_track: Option<&VideoTrack>,
    audio_track: Option<&AudioTrack>,
) -> Result<u32, AppError> {
    let mut timescales = Vec::new();
    if let Some(track) = video_track {
        timescales.push(track.config.timescale);
    }
    if let Some(track) = audio_track {
        timescales.push(track.config.timescale);
    }

    let mut movie_timescale = 1u32;
    for timescale in timescales {
        movie_timescale = lcm_u32(movie_timescale, timescale)
            .ok_or_else(|| AppError::Conversion("无法为 MP4 计算安全的时间基".to_string()))?;
    }

    Ok(movie_timescale.max(1))
}

fn gcd_u32(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn lcm_u32(left: u32, right: u32) -> Option<u32> {
    if left == 0 || right == 0 {
        return Some(left.max(right));
    }

    let gcd = gcd_u32(left, right);
    let value = (left as u64 / gcd as u64).checked_mul(right as u64)?;
    u32::try_from(value).ok()
}

fn parse_transport_stream(
    data: &[u8],
) -> Result<(Option<VideoTrack>, Option<AudioTrack>), AppError> {
    let sync_offset = detect_sync_offset(data)?;
    let mut stream_map = StreamMap::default();
    let mut assemblers: HashMap<u16, PesAssembler> = HashMap::new();
    let mut timestamp_state: HashMap<u16, StreamTimestampState> = HashMap::new();
    let mut video_packets = Vec::new();
    let mut audio_packets = Vec::new();

    for packet in data[sync_offset..].chunks(TS_PACKET_SIZE) {
        if packet.len() != TS_PACKET_SIZE {
            continue;
        }
        if packet[0] != 0x47 {
            continue;
        }

        let payload_unit_start = packet[1] & 0x40 != 0;
        let pid = (((packet[1] & 0x1f) as u16) << 8) | packet[2] as u16;
        let adaptation_field_control = (packet[3] >> 4) & 0x03;

        if adaptation_field_control == 0x00 || adaptation_field_control == 0x02 {
            continue;
        }

        let mut payload_offset = 4usize;
        if adaptation_field_control == 0x03 {
            let adaptation_len = packet[payload_offset] as usize;
            payload_offset += 1 + adaptation_len;
            if payload_offset > TS_PACKET_SIZE {
                continue;
            }
        }

        let payload = &packet[payload_offset..];
        if payload.is_empty() {
            continue;
        }

        if pid == 0 && payload_unit_start {
            if let Some(pmt_pid) = parse_pat(payload) {
                stream_map.pmt_pid = Some(pmt_pid);
            }
            continue;
        }

        if Some(pid) == stream_map.pmt_pid && payload_unit_start {
            parse_pmt(payload, &mut stream_map);
            continue;
        }

        let is_video_pid = stream_map.video_pid == Some(pid);
        let is_audio_pid = stream_map.audio_pid == Some(pid);
        if !is_video_pid && !is_audio_pid {
            continue;
        }

        if payload_unit_start {
            flush_pes_packet(
                pid,
                &mut assemblers,
                &mut video_packets,
                &mut audio_packets,
                &stream_map,
            );

            let (pts, dts, pes_payload) =
                parse_pes_packet(payload, timestamp_state.entry(pid).or_default())?;
            assemblers.insert(
                pid,
                PesAssembler {
                    pts,
                    dts,
                    data: pes_payload.to_vec(),
                },
            );
        } else if let Some(assembler) = assemblers.get_mut(&pid) {
            assembler.data.extend_from_slice(payload);
        }
    }

    let remaining_pids: Vec<u16> = assemblers.keys().copied().collect();
    for pid in remaining_pids {
        flush_pes_packet(
            pid,
            &mut assemblers,
            &mut video_packets,
            &mut audio_packets,
            &stream_map,
        );
    }

    if video_packets.is_empty() && audio_packets.is_empty() {
        return Err(AppError::Conversion(build_stream_type_error(&stream_map)));
    }

    let video_track = if video_packets.is_empty() {
        None
    } else {
        Some(build_video_track(&video_packets)?)
    };

    let audio_track = if audio_packets.is_empty() {
        None
    } else {
        Some(build_audio_track(&audio_packets)?)
    };

    Ok((video_track, audio_track))
}

fn build_stream_type_error(stream_map: &StreamMap) -> String {
    if let Some(stream_type) = stream_map.unsupported_video_stream_type {
        return format!("暂不支持的视频编码流类型 0x{:02x}", stream_type);
    }
    if let Some(stream_type) = stream_map.unsupported_audio_stream_type {
        return format!("暂不支持的音频编码流类型 0x{:02x}", stream_type);
    }
    "TS 文件中未找到 H.264 视频或 AAC 音频流".to_string()
}

fn detect_sync_offset(data: &[u8]) -> Result<usize, AppError> {
    let max_offset = TS_PACKET_SIZE.min(data.len());
    for offset in 0..max_offset {
        let mut matches = 0usize;
        let mut index = offset;
        while index < data.len() && data[index] == 0x47 {
            matches += 1;
            if matches >= 5 {
                return Ok(offset);
            }
            index += TS_PACKET_SIZE;
        }
    }

    Err(AppError::Conversion(
        "输入文件不是有效的 MPEG-TS 数据".to_string(),
    ))
}

fn parse_pat(payload: &[u8]) -> Option<u16> {
    let section = psi_section(payload)?;
    if section.len() < 12 || section[0] != 0x00 {
        return None;
    }

    let section_length = (((section[1] & 0x0f) as usize) << 8) | section[2] as usize;
    let section_end = 3 + section_length;
    if section_end > section.len() || section_end < 12 {
        return None;
    }

    let mut cursor = 8usize;
    let data_end = section_end.saturating_sub(4);
    while cursor + 4 <= data_end {
        let program_number = u16::from_be_bytes([section[cursor], section[cursor + 1]]);
        let pid = (((section[cursor + 2] & 0x1f) as u16) << 8) | section[cursor + 3] as u16;
        if program_number != 0 {
            return Some(pid);
        }
        cursor += 4;
    }

    None
}

fn parse_pmt(payload: &[u8], stream_map: &mut StreamMap) {
    let Some(section) = psi_section(payload) else {
        return;
    };
    if section.len() < 16 || section[0] != 0x02 {
        return;
    }

    let section_length = (((section[1] & 0x0f) as usize) << 8) | section[2] as usize;
    let section_end = 3 + section_length;
    if section_end > section.len() || section_end < 16 {
        return;
    }

    let program_info_length = (((section[10] & 0x0f) as usize) << 8) | section[11] as usize;
    let mut cursor = 12 + program_info_length;
    let data_end = section_end.saturating_sub(4);

    while cursor + 5 <= data_end {
        let stream_type = section[cursor];
        let elementary_pid =
            (((section[cursor + 1] & 0x1f) as u16) << 8) | section[cursor + 2] as u16;
        let es_info_length =
            (((section[cursor + 3] & 0x0f) as usize) << 8) | section[cursor + 4] as usize;

        match stream_type {
            0x1b if stream_map.video_pid.is_none() => stream_map.video_pid = Some(elementary_pid),
            0x0f if stream_map.audio_pid.is_none() => stream_map.audio_pid = Some(elementary_pid),
            0x1b => {}
            0x0f => {}
            0x24 if stream_map.unsupported_video_stream_type.is_none() => {
                stream_map.unsupported_video_stream_type = Some(stream_type);
            }
            0x03 | 0x04 | 0x11 if stream_map.unsupported_audio_stream_type.is_none() => {
                stream_map.unsupported_audio_stream_type = Some(stream_type);
            }
            _ => {}
        }

        cursor += 5 + es_info_length;
    }
}

fn psi_section(payload: &[u8]) -> Option<&[u8]> {
    if payload.is_empty() {
        return None;
    }

    let pointer_field = payload[0] as usize;
    if 1 + pointer_field >= payload.len() {
        return None;
    }

    Some(&payload[1 + pointer_field..])
}

fn parse_pes_packet<'a>(
    payload: &'a [u8],
    timestamp_state: &mut StreamTimestampState,
) -> Result<(Option<u64>, Option<u64>, &'a [u8]), AppError> {
    if payload.len() < 9 || payload[0..3] != [0x00, 0x00, 0x01] {
        return Err(AppError::Conversion(
            "PES 数据头无效，无法转换为 MP4".to_string(),
        ));
    }

    let flags = payload[7];
    let header_length = payload[8] as usize;
    let pts_dts_flags = (flags >> 6) & 0x03;
    let payload_offset = 9 + header_length;
    if payload_offset > payload.len() {
        return Err(AppError::Conversion("PES 数据头长度超出范围".to_string()));
    }

    let mut pts = None;
    let mut dts = None;

    if pts_dts_flags == 0x02 || pts_dts_flags == 0x03 {
        if payload.len() < 14 {
            return Err(AppError::Conversion("PES PTS 解析失败".to_string()));
        }
        let parsed = parse_pts_dts(&payload[9..14])?;
        pts = Some(timestamp_state.pts.unroll(parsed));
    }

    if pts_dts_flags == 0x03 {
        if payload.len() < 19 {
            return Err(AppError::Conversion("PES DTS 解析失败".to_string()));
        }
        let parsed = parse_pts_dts(&payload[14..19])?;
        dts = Some(timestamp_state.dts.unroll(parsed));
    }

    if dts.is_none() {
        dts = pts;
    }

    Ok((pts, dts, &payload[payload_offset..]))
}

fn parse_pts_dts(data: &[u8]) -> Result<u64, AppError> {
    if data.len() < 5 {
        return Err(AppError::Conversion("时间戳字段长度不足".to_string()));
    }

    if data[0] & 0x01 != 0x01 || data[2] & 0x01 != 0x01 || data[4] & 0x01 != 0x01 {
        return Err(AppError::Conversion("时间戳字段校验位无效".to_string()));
    }

    let value = (((data[0] >> 1) & 0x07) as u64) << 30
        | (data[1] as u64) << 22
        | ((data[2] >> 1) as u64) << 15
        | (data[3] as u64) << 7
        | ((data[4] >> 1) as u64);

    Ok(value)
}

fn flush_pes_packet(
    pid: u16,
    assemblers: &mut HashMap<u16, PesAssembler>,
    video_packets: &mut Vec<PesPacket>,
    audio_packets: &mut Vec<PesPacket>,
    stream_map: &StreamMap,
) {
    let Some(assembler) = assemblers.remove(&pid) else {
        return;
    };
    if assembler.data.is_empty() {
        return;
    }

    let packet = PesPacket {
        pts: assembler.pts,
        dts: assembler.dts,
        data: assembler.data,
    };

    if stream_map.video_pid == Some(pid) {
        video_packets.push(packet);
    } else if stream_map.audio_pid == Some(pid) {
        audio_packets.push(packet);
    }
}

fn build_video_track(packets: &[PesPacket]) -> Result<VideoTrack, AppError> {
    let mut raw_samples = Vec::new();
    let mut seq_param_set = None;
    let mut pic_param_set = None;

    for packet in packets {
        let nal_units = split_annex_b_nal_units(&packet.data);
        if nal_units.is_empty() {
            continue;
        }

        let mut sample_bytes = Vec::new();
        let mut is_sync = false;

        for nal in nal_units {
            let nal_ref = RefNal::new(nal, &[], true);
            let nal_type = nal_ref
                .header()
                .map_err(|error| AppError::Conversion(format!("H.264 NAL 解析失败: {:?}", error)))?
                .nal_unit_type();

            if nal_type == UnitType::SeqParameterSet && seq_param_set.is_none() {
                seq_param_set = Some(nal.to_vec());
            } else if nal_type == UnitType::PicParameterSet && pic_param_set.is_none() {
                pic_param_set = Some(nal.to_vec());
            } else if nal_type == UnitType::SliceLayerWithoutPartitioningIdr {
                is_sync = true;
            }

            sample_bytes.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            sample_bytes.extend_from_slice(nal);
        }

        if !sample_bytes.is_empty() {
            raw_samples.push(RawVideoSample {
                pts: packet.pts,
                dts: packet.dts.or(packet.pts),
                is_sync,
                bytes: sample_bytes,
            });
        }
    }

    if raw_samples.is_empty() {
        return Err(AppError::Conversion(
            "未能从 TS 文件中解析出 H.264 视频样本".to_string(),
        ));
    }

    let seq_param_set = seq_param_set.ok_or_else(|| {
        AppError::Conversion("H.264 视频缺少 SPS 参数集，无法生成 MP4".to_string())
    })?;
    let pic_param_set = pic_param_set.ok_or_else(|| {
        AppError::Conversion("H.264 视频缺少 PPS 参数集，无法生成 MP4".to_string())
    })?;

    let (width, height, fps_hint) = parse_sps_metadata(&seq_param_set)?;
    let samples = finalize_video_samples(raw_samples, fps_hint)?;

    let config = TrackConfig {
        track_type: TrackType::Video,
        timescale: MPEG_TS_TIMESCALE,
        language: "und".to_string(),
        media_conf: MediaConfig::AvcConfig(AvcConfig {
            width,
            height,
            seq_param_set,
            pic_param_set,
        }),
    };

    Ok(VideoTrack { config, samples })
}

fn parse_sps_metadata(sps: &[u8]) -> Result<(u16, u16, Option<f64>), AppError> {
    let nal = RefNal::new(sps, &[], true);
    let sps = SeqParameterSet::from_bits(nal.rbsp_bits())
        .map_err(|error| AppError::Conversion(format!("SPS 解析失败: {:?}", error)))?;
    let (width, height) = sps
        .pixel_dimensions()
        .map_err(|error| AppError::Conversion(format!("SPS 尺寸解析失败: {:?}", error)))?;

    let width = u16::try_from(width)
        .map_err(|_| AppError::Conversion("视频宽度超出 MP4 支持范围".to_string()))?;
    let height = u16::try_from(height)
        .map_err(|_| AppError::Conversion("视频高度超出 MP4 支持范围".to_string()))?;

    Ok((width, height, sps.fps()))
}

fn finalize_video_samples(
    raw_samples: Vec<RawVideoSample>,
    fps_hint: Option<f64>,
) -> Result<Vec<Mp4Sample>, AppError> {
    if raw_samples.is_empty() {
        return Ok(Vec::new());
    }

    let mut normalized = Vec::with_capacity(raw_samples.len());
    let mut last_start_time = 0u64;
    let mut last_duration = fps_hint
        .map(fps_hint_to_duration)
        .unwrap_or(LAST_VIDEO_SAMPLE_DURATION_FALLBACK);

    for sample in raw_samples {
        let start_time = sample
            .dts
            .or(sample.pts)
            .unwrap_or(last_start_time.saturating_add(last_duration as u64));
        let pts = sample.pts.unwrap_or(start_time);

        normalized.push((start_time, pts, sample.is_sync, sample.bytes));
        last_start_time = start_time;
    }

    let mut samples = Vec::with_capacity(normalized.len());

    for index in 0..normalized.len() {
        let (start_time, pts, is_sync, bytes) = &normalized[index];
        let duration = if let Some((next_start_time, _, _, _)) = normalized.get(index + 1) {
            let delta = next_start_time.saturating_sub(*start_time);
            if delta == 0 {
                last_duration
            } else {
                u32::try_from(delta).unwrap_or(u32::MAX)
            }
        } else {
            last_duration
        };

        last_duration = duration.max(1);
        let rendering_offset =
            (*pts as i128 - *start_time as i128).clamp(i32::MIN as i128, i32::MAX as i128) as i32;

        samples.push(Mp4Sample {
            start_time: *start_time,
            duration: last_duration,
            rendering_offset,
            is_sync: *is_sync,
            bytes: Bytes::from(bytes.clone()),
        });
    }

    Ok(samples)
}

fn fps_hint_to_duration(fps: f64) -> u32 {
    if !fps.is_finite() || fps <= 0.0 {
        return LAST_VIDEO_SAMPLE_DURATION_FALLBACK;
    }

    let duration = (MPEG_TS_TIMESCALE as f64 / fps).round();
    duration.max(1.0).min(u32::MAX as f64) as u32
}

fn build_audio_track(packets: &[PesPacket]) -> Result<AudioTrack, AppError> {
    let mut samples = Vec::new();
    let mut track_info = None;
    let mut next_sample_time = None;
    let mut total_payload_bytes = 0u64;

    for packet in packets {
        let mut offset = 0usize;
        let mut packet_time = None;

        while offset < packet.data.len() {
            let header = parse_adts_header(&packet.data[offset..])?;
            validate_audio_track_info(track_info, &header)?;
            track_info = Some(AudioTrackInfo {
                profile: header.profile,
                freq_index: header.freq_index,
                channel_config: header.channel_config,
                sample_rate: header.sample_rate,
            });
            if packet_time.is_none() {
                packet_time = packet
                    .pts
                    .map(|pts| scale_timestamp(pts, header.sample_rate, MPEG_TS_TIMESCALE))
                    .or(next_sample_time);
            }

            if offset + header.frame_len > packet.data.len() {
                return Err(AppError::Conversion(
                    "AAC ADTS 帧跨越了 PES 边界，当前转换器无法安全处理".to_string(),
                ));
            }

            let sample_time = packet_time.unwrap_or_else(|| next_sample_time.unwrap_or(0));
            let frame_payload =
                packet.data[offset + header.header_len..offset + header.frame_len].to_vec();
            total_payload_bytes += frame_payload.len() as u64;

            samples.push(Mp4Sample {
                start_time: sample_time,
                duration: AAC_SAMPLES_PER_FRAME,
                rendering_offset: 0,
                is_sync: true,
                bytes: Bytes::from(frame_payload),
            });

            packet_time = Some(sample_time + AAC_SAMPLES_PER_FRAME as u64);
            next_sample_time = packet_time;
            offset += header.frame_len;
        }
    }

    if samples.is_empty() {
        return Err(AppError::Conversion(
            "未能从 TS 文件中解析出 AAC 音频样本".to_string(),
        ));
    }

    let track_info = track_info
        .ok_or_else(|| AppError::Conversion("AAC 轨信息不完整，无法生成 MP4".to_string()))?;

    let total_duration = samples
        .last()
        .map(|last| last.start_time + last.duration as u64)
        .unwrap_or(0);
    let bitrate =
        estimate_audio_bitrate(total_payload_bytes, total_duration, track_info.sample_rate);

    let config = TrackConfig {
        track_type: TrackType::Audio,
        timescale: track_info.sample_rate,
        language: "und".to_string(),
        media_conf: MediaConfig::AacConfig(AacConfig {
            bitrate,
            profile: track_info.profile,
            freq_index: track_info.freq_index,
            chan_conf: track_info.channel_config,
        }),
    };

    Ok(AudioTrack { config, samples })
}

fn validate_audio_track_info(
    existing: Option<AudioTrackInfo>,
    header: &AudioFrameHeader,
) -> Result<(), AppError> {
    let Some(existing) = existing else {
        return Ok(());
    };

    if existing.profile != header.profile
        || existing.freq_index != header.freq_index
        || existing.channel_config != header.channel_config
    {
        return Err(AppError::Conversion(
            "AAC 轨在转换过程中发生了参数切换，当前版本暂不支持".to_string(),
        ));
    }

    Ok(())
}

fn parse_adts_header(data: &[u8]) -> Result<AudioFrameHeader, AppError> {
    if data.len() < 7 {
        return Err(AppError::Conversion("AAC ADTS 头长度不足".to_string()));
    }

    if data[0] != 0xff || (data[1] & 0xf0) != 0xf0 {
        return Err(AppError::Conversion("AAC ADTS 同步字无效".to_string()));
    }

    let protection_absent = data[1] & 0x01 != 0;
    let header_len = if protection_absent { 7 } else { 9 };
    let profile = AudioObjectType::try_from(((data[2] & 0xc0) >> 6) + 1)
        .map_err(|error| AppError::Conversion(format!("AAC profile 不支持: {}", error)))?;
    let freq_index = SampleFreqIndex::try_from((data[2] & 0x3c) >> 2)
        .map_err(|error| AppError::Conversion(format!("AAC 采样率不支持: {}", error)))?;
    let channel_value = ((data[2] & 0x01) << 2) | ((data[3] & 0xc0) >> 6);
    let channel_config = ChannelConfig::try_from(channel_value)
        .map_err(|error| AppError::Conversion(format!("AAC 声道布局不支持: {}", error)))?;

    let frame_len = (((data[3] & 0x03) as usize) << 11)
        | ((data[4] as usize) << 3)
        | (((data[5] & 0xe0) as usize) >> 5);

    if frame_len <= header_len {
        return Err(AppError::Conversion("AAC ADTS 帧长度无效".to_string()));
    }

    Ok(AudioFrameHeader {
        header_len,
        frame_len,
        profile,
        freq_index,
        channel_config,
        sample_rate: freq_index.freq(),
    })
}

fn estimate_audio_bitrate(total_payload_bytes: u64, total_duration: u64, sample_rate: u32) -> u32 {
    if total_duration == 0 || sample_rate == 0 {
        return 0;
    }

    let bits = total_payload_bytes.saturating_mul(8);
    let per_second = bits.saturating_mul(sample_rate as u64) / total_duration;
    per_second.min(u32::MAX as u64) as u32
}

fn scale_timestamp(value: u64, to_scale: u32, from_scale: u32) -> u64 {
    if to_scale == from_scale {
        return value;
    }

    let scaled = (value as u128)
        .saturating_mul(to_scale as u128)
        .saturating_add((from_scale / 2) as u128)
        / from_scale as u128;
    scaled.min(u64::MAX as u128) as u64
}

fn split_annex_b_nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut cursor = 0usize;

    while let Some(start) = find_start_code(data, cursor) {
        let nal_start = start + start_code_len(&data[start..]);
        let next_start = find_start_code(data, nal_start).unwrap_or(data.len());
        let mut nal_end = next_start;

        if next_start == data.len() {
            while nal_end > nal_start && data[nal_end - 1] == 0x00 {
                nal_end -= 1;
            }
        }

        if nal_start < nal_end {
            units.push(&data[nal_start..nal_end]);
        }

        cursor = next_start;
    }

    units
}

fn find_start_code(data: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    while index + 3 < data.len() {
        if data[index] == 0x00 && data[index + 1] == 0x00 {
            if data[index + 2] == 0x01 {
                return Some(index);
            }
            if index + 3 < data.len() && data[index + 2] == 0x00 && data[index + 3] == 0x01 {
                return Some(index);
            }
        }
        index += 1;
    }

    None
}

fn start_code_len(data: &[u8]) -> usize {
    if data.len() >= 4 && data[0..4] == [0x00, 0x00, 0x00, 0x01] {
        4
    } else {
        3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_annex_b_nal_units_supports_mixed_start_codes() {
        let data = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x1f, 0x00, 0x00, 0x01, 0x68, 0xee, 0x3c,
            0x80,
        ];

        let nal_units = split_annex_b_nal_units(&data);
        assert_eq!(nal_units.len(), 2);
        assert_eq!(nal_units[0], &[0x67, 0x64, 0x00, 0x1f]);
        assert_eq!(nal_units[1], &[0x68, 0xee, 0x3c, 0x80]);
    }

    #[test]
    fn parse_adts_header_extracts_basic_audio_config() {
        let header = [0xff, 0xf1, 0x50, 0x80, 0x0d, 0x7f, 0xfc];
        let parsed = parse_adts_header(&header).expect("valid ADTS header");

        assert_eq!(parsed.header_len, 7);
        assert_eq!(parsed.frame_len, 107);
        assert_eq!(parsed.profile, AudioObjectType::AacLowComplexity);
        assert_eq!(parsed.freq_index, SampleFreqIndex::Freq44100);
        assert_eq!(parsed.channel_config, ChannelConfig::Stereo);
        assert_eq!(parsed.sample_rate, 44_100);
    }

    #[test]
    fn scale_timestamp_rounds_to_target_timescale() {
        assert_eq!(scale_timestamp(90_000, 44_100, 90_000), 44_100);
        assert_eq!(scale_timestamp(45_000, 48_000, 90_000), 24_000);
    }
}
