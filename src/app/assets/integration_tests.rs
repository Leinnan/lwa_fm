//! Opt-in media tests use the actual tools and application sampler, not a mock.
use super::*;

struct Fixture(PathBuf);
struct BenchmarkProcessLimit;
impl Drop for BenchmarkProcessLimit {
    fn drop(&mut self) {
        process::BENCHMARK_PROCESS_LIMIT.store(1, Ordering::Release);
    }
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "lwa_fm_video_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("fixture directory");
        Self(path)
    }
    fn video(&self, name: &str, filter: &str, duration: &str) -> SourceKey {
        self.video_with_gop(name, filter, duration, "30")
    }
    fn video_with_gop(&self, name: &str, filter: &str, duration: &str, gop: &str) -> SourceKey {
        let path = self.0.join(name);
        let mut command = Command::new(ffmpeg_sidecar::paths::ffmpeg_path());
        command
            .args([
                "-v",
                "error",
                "-nostdin",
                "-y",
                "-filter_threads",
                "1",
                "-f",
                "lavfi",
                "-i",
                filter,
                "-t",
                duration,
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                gop,
                "-threads",
                "1",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path);
        process::run(
            &mut command,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(10),
            1024,
        )
        .expect("generate video fixture");
        Self::key(&path)
    }
    fn key(path: &Path) -> SourceKey {
        let metadata = fs::metadata(path).expect("video metadata");
        SourceKey::new(
            path,
            metadata
                .modified()
                .expect("modified")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("revision")
                .as_nanos(),
            metadata.len(),
        )
    }
}

#[test]
#[ignore = "FFmpeg performance experiment; run with --ignored --test-threads=1"]
#[allow(clippy::too_many_lines)] // Reports transport, threads, and scheduling in one experiment.
fn benchmark_transport_and_decoder_threads() {
    let fixture = Fixture::new();
    let source = std::env::var("LWA_THUMBNAIL_BENCHMARK_SOURCE").map_or_else(
        |_| fixture.video_with_gop("matrix.mp4", "testsrc2=size=1280x720:rate=30", "10", "300"),
        |path| Fixture::key(Path::new(&path)),
    );
    let info = probe(
        &source,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(3),
    )
    .expect("asset fixture operation");
    let times = sample_times(info.duration.unwrap_or(10.0));
    let mut measurements = Vec::new();
    for edge in [160, 480] {
        for threads in [1, 2, 4] {
            for transport in [FrameTransport::Png, FrameTransport::Ppm] {
                let mut trials = Vec::new();
                for _ in 0..3 {
                    let started = Instant::now();
                    for index in [0, 6, 11] {
                        let mut command =
                            ffmpeg_with_threads(&source, &info, times[index], edge, None, threads);
                        command.args(["-frames:v", "1"]);
                        let samples = decode_samples_with_transport(
                            &mut command,
                            times[index],
                            edge,
                            &AtomicBool::new(false),
                            Instant::now() + Duration::from_secs(5),
                            transport,
                        )
                        .expect("asset fixture operation");
                        assert_eq!(samples.len(), 1);
                    }
                    trials.push(started.elapsed().as_secs_f64() * 1000.0);
                }
                measurements.push(serde_json::json!({"edge":edge,"threads":threads,"transport":format!("{transport:?}"),"three_seek_ms":trials}));
            }
        }
    }
    eprintln!(
        "TRANSPORT_BENCHMARK={}",
        serde_json::json!({"source":source.path,"measurements":measurements})
    );

    let mut strategies = Vec::new();
    for strategy in ["seeks", "sequential", "two_processes"] {
        let mut trials = Vec::new();
        for _ in 0..3 {
            let started = Instant::now();
            if strategy == "sequential" {
                let duration = info.duration.unwrap_or(10.0);
                let selection = format!("select='gte(t,selected_n*{:.9})'", duration * 0.85 / 11.0);
                let mut command =
                    ffmpeg_with_threads(&source, &info, times[0], 240, Some(&selection), 2);
                command.args(["-frames:v", "12"]);
                assert_eq!(
                    decode_samples(
                        &mut command,
                        times[0],
                        240,
                        &AtomicBool::new(false),
                        Instant::now() + Duration::from_secs(8)
                    )
                    .expect("asset fixture operation")
                    .len(),
                    12
                );
            } else {
                let workers = if strategy == "two_processes" { 2 } else { 1 };
                let _reset_limit = BenchmarkProcessLimit;
                process::BENCHMARK_PROCESS_LIMIT.store(workers, Ordering::Release);
                let handles: Vec<_> = (0..workers)
                    .map(|worker| {
                        let source = source.clone();
                        let info = info.clone();
                        let times = times.clone();
                        std::thread::spawn(move || {
                            for index in (worker..times.len()).step_by(workers) {
                                let mut command =
                                    ffmpeg_with_threads(&source, &info, times[index], 240, None, 2);
                                command.args(["-frames:v", "1"]);
                                assert_eq!(
                                    decode_samples(
                                        &mut command,
                                        times[index],
                                        240,
                                        &AtomicBool::new(false),
                                        Instant::now() + Duration::from_secs(5)
                                    )
                                    .expect("asset fixture operation")
                                    .len(),
                                    1
                                );
                            }
                        })
                    })
                    .collect();
                for handle in handles {
                    handle.join().expect("asset fixture operation");
                }
            }
            trials.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        strategies.push(serde_json::json!({"strategy":strategy,"overview_ms":trials}));
    }
    eprintln!(
        "STRATEGY_BENCHMARK={}",
        serde_json::json!({"source":source.path,"measurements":strategies})
    );
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn generate(source: SourceKey, spec: PreviewSpec) -> (DecodedAnimation, Vec<DecodedAnimation>) {
    let mut task = VideoTask::new(source, spec, AnimatedSourceKind::Video);
    let mut progress = Vec::new();
    loop {
        match task.step(&AtomicBool::new(false)).expect("sample video") {
            PreviewStep::Complete(animation) => {
                flush_cache();
                return (animation, progress);
            }
            PreviewStep::Continue(Some(animation)) => progress.push(animation),
            PreviewStep::Continue(None) => {}
        }
    }
}

#[test]
#[ignore = "requires FFmpeg and FFprobe; run with --ignored --test-threads=1"]
fn real_video_overview_cache_and_poster_reuse() {
    let fixture = Fixture::new();
    let source = fixture.video("long.mp4", "testsrc2=size=320x180:rate=30", "20");
    let spec = PreviewSpec::new(160);
    let fallback = probe_with_ffmpeg(
        &source,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(2),
    )
    .expect("FFmpeg metadata fallback");
    assert_eq!(fallback.duration, Some(20.0));
    assert_eq!(fallback.stream, 0);
    let started = Instant::now();
    let (animation, progress) = generate(source.clone(), spec);
    assert_eq!(animation.frames.len(), 12);
    assert_eq!(progress.len(), 1);
    assert_eq!(progress[0].frames.len(), 4);
    assert_eq!(progress[0].source_timestamps[0], Duration::from_secs(1));
    assert_eq!(progress[0].source_timestamps[3], Duration::from_secs(18));
    assert_eq!(animation.frames[0].width, 160);
    assert_eq!(animation.frames[0].height, 90);
    assert!(
        animation
            .frames
            .windows(2)
            .any(|frames| frames[0].rgba != frames[1].rgba)
    );
    assert!(animation.byte_len() <= MAX_DECODED);
    let colors: std::collections::HashSet<_> = animation.frames[0]
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect();
    assert!(colors.len() > 256, "overview must retain full color");
    save_overview_sheet(&animation);
    eprintln!(
        "20-second sparse overview: {:?}, {} decoded bytes",
        started.elapsed(),
        animation.byte_len()
    );
    let count = process::PROCESS_COUNT.load(Ordering::Relaxed);
    let (cached, _) = generate(source.clone(), spec);
    assert_eq!(cached.frames.len(), 12);
    assert_eq!(
        process::PROCESS_COUNT.load(Ordering::Relaxed),
        count,
        "disk hit must start no process"
    );
    let from_overview =
        poster(&source, 96, &AtomicBool::new(false)).expect("reuse overview as poster");
    assert_eq!((from_overview.width, from_overview.height), (96, 54));
    assert_eq!(
        process::PROCESS_COUNT.load(Ordering::Relaxed),
        count,
        "poster reuse must start no process"
    );
    fs::write(
        preview_cache_path(&source, spec, AnimatedSourceKind::Video),
        b"corrupt",
    )
    .expect("corrupt cache");
    assert_eq!(generate(source, spec).0.frames.len(), 12);
}

#[test]
#[ignore = "requires FFmpeg and FFprobe; run with --ignored --test-threads=1"]
fn real_short_video_and_geometry() {
    let fixture = Fixture::new();
    let portrait = fixture.video("short.mp4", "testsrc2=size=180x320:rate=30", "0.5");
    let image = poster(&portrait, 96, &AtomicBool::new(false)).expect("short poster");
    assert_eq!((image.width, image.height), (54, 96));
    let metadata: crate::data::files::DirEntryMetaData = fs::metadata(&portrait.path)
        .expect("portrait metadata")
        .into();
    let cached_poster = super::super::load_or_generate_thumbnail(
        &portrait.path,
        super::super::ThumbnailKind::Video,
        super::super::IconSize::Small,
        96,
        metadata.source_revision,
        metadata.size,
        &AtomicBool::new(false),
    )
    .expect("poster cache miss");
    flush_cache();
    let path = cache::thumbnail_cache_path(
        &portrait.path,
        super::super::IconSize::Small,
        96,
        metadata.source_revision,
        metadata.size,
    );
    assert!(
        image::open(path).is_ok(),
        "background poster cache must contain a decodable JPEG"
    );
    assert_eq!((cached_poster.width, cached_poster.height), (54, 96));
    let (short, _) = generate(portrait, PreviewSpec::new(160));
    assert_eq!(short.frames.len(), 12);
    assert!(
        short
            .frames
            .iter()
            .all(|frame| frame.width == 90 && frame.height == 160)
    );
    assert!(
        short
            .source_timestamps
            .iter()
            .all(|time| *time < Duration::from_millis(500))
    );

    let anamorphic = fixture.video("sar.mp4", "testsrc2=size=320x240:rate=30,setsar=2", "0.5");
    let image = poster(&anamorphic, 160, &AtomicBool::new(false)).expect("anamorphic poster");
    assert_eq!((image.width, image.height), (160, 60));
    let rotated = fixture.0.join("rotated.mp4");
    let mut command = Command::new(ffmpeg_sidecar::paths::ffmpeg_path());
    command
        .args([
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-display_rotation",
            "90",
            "-i",
        ])
        .arg(&anamorphic.path)
        .args(["-c", "copy"])
        .arg(&rotated);
    process::run(
        &mut command,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(2),
        1024,
    )
    .expect("rotation fixture");
    let image =
        poster(&Fixture::key(&rotated), 160, &AtomicBool::new(false)).expect("rotated poster");
    assert_eq!((image.width, image.height), (60, 160));
}

#[test]
#[ignore = "requires FFmpeg and FFprobe; run with --ignored --test-threads=1"]
fn real_sparse_frames_and_audio_tail() {
    let fixture = Fixture::new();
    let single = fixture.video("single.mp4", "color=c=red:size=64x64:rate=1", "1");
    assert_eq!(generate(single, PreviewSpec::new(160)).0.frames.len(), 1);
    let slow = fixture.video("slow.mp4", "testsrc2=size=64x64:rate=1", "8");
    let animation = generate(slow, PreviewSpec::new(160)).0;
    assert_eq!(animation.frames.len(), 7);
    assert!(
        (animation
            .source_timestamps
            .last()
            .expect("asset fixture operation")
            .as_secs_f64()
            - 7.0)
            .abs()
            < 0.002
    );
    let video = fixture.video("video.mp4", "testsrc2=size=64x64:rate=30", "0.5");
    let tail = fixture.0.join("tail.mkv");
    let mut command = Command::new(ffmpeg_sidecar::paths::ffmpeg_path());
    command
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(&video.path)
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=10",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-threads",
            "1",
        ])
        .arg(&tail);
    process::run(
        &mut command,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(5),
        1024,
    )
    .expect("asset fixture operation");
    let source = Fixture::key(&tail);
    assert_eq!(
        probe(
            &source,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(2)
        )
        .expect("asset fixture operation")
        .duration,
        Some(0.5)
    );
    let animation = generate(source, PreviewSpec::new(160)).0;
    assert!(!animation.frames.is_empty());
    assert!(
        animation
            .source_timestamps
            .iter()
            .all(|time| time.as_secs_f64() < 0.501)
    );
}

#[test]
#[ignore = "requires FFmpeg and FFprobe; run with --ignored --test-threads=1"]
fn real_disk_poster_reuse_preserves_timestamp_and_avoids_first_decode() {
    let fixture = Fixture::new();
    let source = fixture.video("reuse.mp4", "testsrc2=size=320x180:rate=30", "20");
    let image = super::super::load_or_generate_thumbnail(
        &source.path,
        super::super::ThumbnailKind::Video,
        super::super::IconSize::Small,
        160,
        source.revision,
        source.size,
        &AtomicBool::new(false),
    )
    .expect("asset fixture operation");
    flush_cache();
    POSTERS.lock().expect("asset fixture operation").clear();
    PROBES.lock().expect("asset fixture operation").clear();
    let before = process::PROCESS_COUNT.load(Ordering::Relaxed);
    let sample =
        cached_poster(&source, 160, &AtomicBool::new(false)).expect("asset fixture operation");
    assert_eq!(sample.timestamp, Duration::from_secs(1));
    assert_eq!(
        (sample.frame.width, sample.frame.height),
        (image.width, image.height)
    );
    assert_eq!(process::PROCESS_COUNT.load(Ordering::Relaxed), before);
    POSTERS.lock().expect("asset fixture operation").clear();
    let (animation, progress) = generate(source, PreviewSpec::new(160));
    assert_eq!(animation.frames.len(), 12);
    assert_eq!(progress[0].frames.len(), 1);
    assert_eq!(
        process::PROCESS_COUNT.load(Ordering::Relaxed) - before,
        12,
        "one probe plus eleven samples"
    );
}

fn save_overview_sheet(animation: &DecodedAnimation) {
    if let Ok(directory) = std::env::var("LWA_THUMBNAIL_ARTIFACT_DIR") {
        let mut sheet = image::RgbaImage::new(640, 270);
        for (index, frame) in animation.frames.iter().enumerate() {
            let image = image::RgbaImage::from_raw(
                frame.width as u32,
                frame.height as u32,
                frame.rgba.to_vec(),
            )
            .expect("frame pixels");
            image::imageops::replace(
                &mut sheet,
                &image,
                i64::try_from(index % 4 * 160).expect("column"),
                i64::try_from(index / 4 * 90).expect("row"),
            );
        }
        sheet
            .save(Path::new(&directory).join("thumbnail-overview.png"))
            .expect("save overview artifact");
    }
}

#[test]
#[ignore = "requires FFmpeg and FFprobe; run with --ignored --test-threads=1"]
fn real_cost_based_batch_reuses_poster_without_duplicate_frames() {
    let fixture = Fixture::new();
    let source = fixture.video("batch.mp4", "testsrc2=size=1280x720:rate=30", "10");
    super::super::load_or_generate_thumbnail(
        &source.path,
        super::super::ThumbnailKind::Video,
        super::super::IconSize::Small,
        160,
        source.revision,
        source.size,
        &AtomicBool::new(false),
    )
    .expect("batch poster");
    flush_cache();
    POSTERS.lock().expect("poster cache").clear();
    PROBES.lock().expect("probe cache").clear();
    let before = process::PROCESS_COUNT.load(Ordering::Relaxed);
    let (animation, progress) = generate(source, PreviewSpec::new(160));
    assert_eq!(animation.frames.len(), 12);
    assert_eq!(progress[0].frames.len(), 1);
    assert!(
        animation
            .source_timestamps
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    );
    assert_eq!(
        process::PROCESS_COUNT.load(Ordering::Relaxed) - before,
        2,
        "one probe and one sequential batch"
    );
}

#[test]
#[ignore = "requires FFmpeg and FFprobe; run with --ignored --test-threads=1"]
fn real_hdr_sources_use_available_conversion_or_a_bounded_fallback() {
    let fixture = Fixture::new();
    for transfer in ["smpte2084", "arib-std-b67"] {
        let path = fixture.0.join(format!("{transfer}.mkv"));
        let mut command = Command::new(ffmpeg_sidecar::paths::ffmpeg_path());
        command
            .args([
                "-v",
                "error",
                "-nostdin",
                "-y",
                "-filter_threads",
                "1",
                "-f",
                "lavfi",
                "-i",
                "color=c=gray:s=64x64:r=1",
                "-frames:v",
                "1",
                "-c:v",
                "ffv1",
                "-pix_fmt",
                "yuv420p10le",
            ])
            .arg("-vf")
            .arg(format!(
                "setparams=color_primaries=bt2020:color_trc={transfer}:colorspace=bt2020nc"
            ))
            .arg(&path);
        process::run(
            &mut command,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(3),
            1024,
        )
        .expect("HDR fixture");
        let source = Fixture::key(&path);
        let info = probe(
            &source,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(3),
        )
        .expect("HDR probe");
        assert!(info.hdr);
        eprintln!(
            "{transfer}: HDR tone-map filters available={}",
            info.tone_map
        );
        let frame = poster(&source, 32, &AtomicBool::new(false)).expect("HDR poster or fallback");
        assert_eq!((frame.width, frame.height), (32, 32));
        for pixel in frame.rgba.as_chunks::<4>().0 {
            assert!(
                pixel[0].abs_diff(pixel[1]) <= 2 && pixel[1].abs_diff(pixel[2]) <= 2,
                "neutral HDR must remain neutral after conversion: {pixel:?}"
            );
            assert!(
                (1..255).contains(&pixel[0]),
                "gray must not clip to black or white"
            );
        }
    }
}

#[test]
#[ignore = "provide LWA_THUMBNAIL_BENCHMARK_SOURCE and run this test explicitly"]
fn benchmark_external_video_overview() {
    let path =
        PathBuf::from(std::env::var("LWA_THUMBNAIL_BENCHMARK_SOURCE").expect("benchmark source"));
    let source = Fixture::key(&path);
    let spec = PreviewSpec::new(240);
    let cache_path = preview_cache_path(&source, spec, AnimatedSourceKind::Video);
    let mut measurements = Vec::new();
    for _ in 0..3 {
        let _ = fs::remove_file(&cache_path);
        POSTERS.lock().expect("poster cache").clear();
        PROBES.lock().expect("probe cache").clear();
        let count = process::PROCESS_COUNT.load(Ordering::Relaxed);
        let started = Instant::now();
        let mut task = VideoTask::new(source.clone(), spec, AnimatedSourceKind::Video);
        let mut coarse_ms = None;
        let animation = loop {
            match task
                .step(&AtomicBool::new(false))
                .expect("benchmark preview")
            {
                PreviewStep::Complete(animation) => break animation,
                PreviewStep::Continue(Some(_)) => {
                    coarse_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                }
                PreviewStep::Continue(None) => {}
            }
        };
        let ready_ms = started.elapsed().as_secs_f64() * 1000.0;
        flush_cache();
        let persisted_ms = started.elapsed().as_secs_f64() * 1000.0;
        let processes = process::PROCESS_COUNT.load(Ordering::Relaxed) - count;
        let started = Instant::now();
        assert_eq!(
            generate(source.clone(), spec).0.frames.len(),
            animation.frames.len()
        );
        let disk_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(
            process::PROCESS_COUNT.load(Ordering::Relaxed) - count,
            processes
        );
        measurements.push(serde_json::json!({"coarse_ms":coarse_ms,"ready_ms":ready_ms,"persisted_ms":persisted_ms,
            "disk_ms":disk_ms,"processes":processes,"decoded_bytes":animation.byte_len(),"cache_bytes":fs::metadata(&cache_path).expect("cache").len()}));
    }
    eprintln!(
        "THUMBNAIL_BENCHMARK={}",
        serde_json::json!({"source":path,"trials":measurements})
    );
    let _ = fs::remove_file(cache_path);
}
