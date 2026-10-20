//! E1 之二：阶梯、清单、选流、路径校验这些**规则**本身的断言。
//!
//! 这些全是纯函数，不需要 ffmpeg。转码产物的形状（分片命名、档位目录、
//! master 引用关系）在 `video_ingest.rs` 的 `--ignored` 用例里跑真 ffmpeg 验收，
//! 但"规则"必须能在任何机器上跑——CI 的 windows runner 就没有 ffmpeg。

use wxlike_video::{
    check_rendition, check_segment_name, master_playlist, rendition_heights, select_rendition,
    usable_bandwidth, Rendition, LADDER, SELECTION_HEADROOM_PCT,
};

fn r(height: u32, width: u32, bps: u64) -> Rendition {
    Rendition {
        height,
        width,
        bandwidth_bps: bps,
        playlist: format!("{height}/playlist.m3u8"),
    }
}

#[test]
fn ladder_never_upsamples() {
    // 只降不升：阶梯里 ≤ 源高的项才产出
    assert_eq!(rendition_heights(1080), vec![360, 720, 1080]);
    assert_eq!(rendition_heights(720), vec![360, 720]);
    assert_eq!(rendition_heights(480), vec![360]);
    assert_eq!(rendition_heights(360), vec![360]);
    assert_eq!(rendition_heights(1440), vec![360, 720, 1080]);
    assert_eq!(
        rendition_heights(1081),
        vec![360, 720, 1080],
        "刚好越过一档不多产一档"
    );
}

#[test]
fn ladder_below_the_smallest_step_keeps_the_source_height() {
    // 240p 的源：不能上采样到 360，保留源高
    assert_eq!(rendition_heights(240), vec![240]);
    // 比 libx264 能编的最小边还矮时，夹到 16（保证有一档可产）
    assert_eq!(rendition_heights(4), vec![16]);
    assert_eq!(rendition_heights(0), vec![16]);
    // 结果永远升序、无重复——master 的档位顺序就是这么来的
    let heights = rendition_heights(720);
    assert!(heights.windows(2).all(|w| w[0] < w[1]), "{heights:?}");
    assert!(heights.iter().all(|h| LADDER.contains(h) || *h < LADDER[0]));
}

#[test]
fn master_declares_every_rendition_in_order() {
    let renditions = [r(360, 640, 900_000), r(720, 1280, 2_400_000)];
    let text = master_playlist(&renditions);
    let lines: Vec<&str> = text.lines().collect();

    assert_eq!(lines[0], "#EXTM3U", "清单第一行必须是 #EXTM3U");
    assert_eq!(lines[1], "#EXT-X-VERSION:3");
    assert!(
        text.contains("#EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=640x360,NAME=\"360p\""),
        "{text}"
    );
    assert!(text.contains("360/playlist.m3u8"), "{text}");
    assert!(text.contains("720/playlist.m3u8"), "{text}");

    // 升序：低档在前，播放器按自己策略往后爬
    let first = lines
        .iter()
        .position(|l| l.starts_with("#EXT-X-STREAM-INF"))
        .unwrap();
    let second = lines[first + 2..]
        .iter()
        .position(|l| l.starts_with("#EXT-X-STREAM-INF"))
        .unwrap()
        + first
        + 2;
    assert!(lines[first].contains("360p") && lines[second].contains("720p"));
    // 每条 STREAM-INF 的下一行必须是变体 URI（相对 hls/ 的路径）
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("#EXT-X-STREAM-INF") {
            assert!(
                lines[i + 1].ends_with("/playlist.m3u8"),
                "变体 URI 紧跟声明: {lines:?}"
            );
        }
    }
    // 空阶梯不是一句 `#EXTM3U` 就完事：那是份合法但无变体的清单，不能崩
    assert_eq!(master_playlist(&[]), "#EXTM3U\n#EXT-X-VERSION:3\n");
}

#[test]
fn selection_picks_the_highest_rendition_that_fits() {
    let renditions = [
        r(360, 640, 900_000),
        r(720, 1280, 2_400_000),
        r(1080, 1920, 5_000_000),
    ];

    // 8 Mbps 可用：留两成余量后 6.4M，1080p（5M）放得下
    assert_eq!(
        usable_bandwidth(8_000_000),
        8_000_000 * SELECTION_HEADROOM_PCT / 100
    );
    assert_eq!(
        select_rendition(&renditions, 8_000_000).unwrap().height,
        1080
    );
    // 4 Mbps：可用 3.2M，只有 720p 放得下
    assert_eq!(
        select_rendition(&renditions, 4_000_000).unwrap().height,
        720
    );
    // 1 Mbps：可用 800k，900k 的 360p 也放不下 → 给最低档（宁糊不黑）
    assert_eq!(
        select_rendition(&renditions, 1_000_000).unwrap().height,
        360
    );
    // 边界：正好等于带宽上限
    assert_eq!(
        select_rendition(&renditions, 1_125_000).unwrap().height,
        360
    );
    // 顺序无关（客户端可能乱序传上来）
    let shuffled = [
        r(1080, 1920, 5_000_000),
        r(360, 640, 900_000),
        r(720, 1280, 2_400_000),
    ];
    assert_eq!(select_rendition(&shuffled, 4_000_000).unwrap().height, 720);
    // 空阶梯没有"最低档"可给：None，而不是编一个 0p
    assert!(select_rendition(&[], 8_000_000).is_none());
}

#[test]
fn rendition_dir_and_segment_names_are_validated_before_any_path() {
    // 档位目录：2–4 位数字（360/720/1080）
    assert!(check_rendition("1080").is_ok());
    assert!(check_rendition("360").is_ok());
    assert!(check_rendition("").is_err());
    assert!(check_rendition("1").is_err());
    assert!(check_rendition("12345").is_err());
    assert!(check_rendition("..").is_err());
    assert!(check_rendition("720/../..").is_err());

    // 分片名：单档与多档两种形状
    assert!(check_segment_name("seg_000.ts").is_ok());
    assert!(check_segment_name("720/seg_012.ts").is_ok());
    assert!(check_segment_name("../hls/playlist.m3u8").is_err());
    assert!(check_segment_name("720/../../meta.json").is_err());
    assert!(check_segment_name("720/360/seg_000.ts").is_err());
    assert!(check_segment_name("seg_000.mp4").is_err());
    assert!(check_segment_name("").is_err());
    // 长度上限（防止用超长名字把路径拼到别处去）
    assert!(check_segment_name(&format!("{}.ts", "a".repeat(64))).is_err());
}
