//! Opt-in media tests use the actual tools and application sampler, not a mock.
use super::*;

struct Fixture(PathBuf);
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
                "30",
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
    let source = fixture.video("long.mp4", "testsrc2=size=320x180:rate=30", "10");
    let spec = PreviewSpec::new(160);
    let fallback = probe_with_ffmpeg(
        &source,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(2),
    )
    .expect("FFmpeg metadata fallback");
    assert_eq!(fallback.duration, Some(10.0));
    assert_eq!(fallback.stream, 0);
    let started = Instant::now();
    let (animation, progress) = generate(source.clone(), spec);
    assert_eq!(animation.frames.len(), 12);
    assert_eq!(progress.len(), 1);
    assert_eq!(progress[0].frames.len(), 4);
    assert_eq!(progress[0].source_timestamps[0], Duration::from_millis(500));
    assert_eq!(progress[0].source_timestamps[3], Duration::from_secs(9));
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
        "10-second sparse overview: {:?}, {} decoded bytes",
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
