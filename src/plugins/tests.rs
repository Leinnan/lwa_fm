//! The bundled plugins, run through the real sandbox and host API.

use super::permissions::PermissionSet;
use super::sandbox::{self, JobLink, Vm};
use super::{CallContext, PluginCode, TargetInfo};
use mlua::{Table, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

fn bundled(id: &str) -> Arc<PluginCode> {
    super::discovery::bundled()
        .into_iter()
        .find(|code| code.id() == id)
        .unwrap_or_else(|| panic!("bundled plugin {id}"))
}

fn vm(code: &Arc<PluginCode>, job: Option<JobLink>) -> Vm {
    super::DISCARD_MESSAGES.set(true);
    let permissions = PermissionSet::from_manifest(&code.manifest.permissions);
    sandbox::create(code, permissions, Arc::default(), job).expect("plugin loads")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dirfleet_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn target(path: &Path) -> TargetInfo {
    let metadata = std::fs::metadata(path).expect("target exists");
    TargetInfo {
        path: path.to_path_buf(),
        is_dir: metadata.is_dir(),
        size: metadata.len(),
        modified: 0,
    }
}

/// `lib/args.lua` of the video converter, called with a Lua options table.
fn ffmpeg_args(options: &str) -> Vec<String> {
    let code = bundled("io.github.leinnan.video-convert");
    let vm = vm(&code, None);
    vm.lua
        .load(format!(
            "return require('lib.args').build('/v/in.mov', '/v/out.ext', {options})"
        ))
        .eval::<Vec<String>>()
        .expect("arguments")
}

#[test]
fn video_args_default_mp4() {
    let args = ffmpeg_args("{ format = 'mp4' }").join(" ");
    assert_eq!(
        args,
        "-i /v/in.mov -c:v libx264 -crf 23 -preset medium -pix_fmt yuv420p -c:a aac -b:a 192k -movflags +faststart -n /v/out.ext"
    );
}

#[test]
fn video_args_fragment_scale_and_no_audio() {
    let args = ffmpeg_args(
        "{ format = 'mkv', quality = 'high', scale = '720', audio = false, start = 1.5, finish = 4, overwrite = true }",
    )
    .join(" ");
    assert!(
        args.starts_with("-ss 1.500 -to 4.000 -i /v/in.mov"),
        "{args}"
    );
    assert!(args.contains("-crf 18 -preset slow"), "{args}");
    assert!(args.contains("-vf scale=-2:720 -an"), "{args}");
    assert!(!args.contains("faststart"), "{args}");
    assert!(args.ends_with("-y /v/out.ext"), "{args}");
}

#[test]
fn video_args_other_formats() {
    let webm = ffmpeg_args("{ format = 'webm', quality = 'fast' }").join(" ");
    assert!(webm.contains("-c:v libvpx-vp9 -b:v 0 -crf 38"), "{webm}");
    assert!(webm.contains("-c:a libopus"), "{webm}");
    let gif = ffmpeg_args("{ format = 'gif', scale = '480' }").join(" ");
    assert!(
        gif.contains("palettegen") && gif.contains("scale=-2:480") && gif.contains("-an"),
        "{gif}"
    );
    let mp3 = ffmpeg_args("{ format = 'mp3' }").join(" ");
    assert!(mp3.contains("-vn -c:a libmp3lame -q:a 2"), "{mp3}");
}

/// `lib/args.lua` of the HEIC converter: `function` called with a Lua options table.
fn heic_args(function: &str, options: &str) -> String {
    let code = bundled("io.github.leinnan.heic-convert");
    let vm = vm(&code, None);
    vm.lua
        .load(format!(
            "return require('lib.args').{function}('/p/in.heic', '/p/out.ext', {options})"
        ))
        .eval::<Vec<String>>()
        .expect("arguments")
        .join(" ")
}

#[test]
fn heic_args_ffmpeg() {
    assert_eq!(
        heic_args("ffmpeg", "{ format = 'jpeg', side = 'src' }"),
        "-i /p/in.heic -frames:v 1 -update 1 -c:v mjpeg -q:v 4 -n /p/out.ext"
    );
    assert_eq!(
        heic_args("ffmpeg", "{ format = 'png', overwrite = true }"),
        "-i /p/in.heic -frames:v 1 -update 1 -y /p/out.ext"
    );
    let scale =
        "scale='min(iw,2048)':'min(ih,2048)':force_original_aspect_ratio=decrease:flags=lanczos";
    let grid = heic_args(
        "ffmpeg",
        "{ format = 'jpeg', quality = 'best', side = '2048', grid = true }",
    );
    assert!(
        grid.starts_with(&format!("-i /p/in.heic -filter_complex [0:g:0]{scale} ")),
        "{grid}"
    );
    assert!(grid.contains("-q:v 2"), "{grid}");
    let plain = heic_args("ffmpeg", "{ format = 'tiff', side = 2048 }");
    assert!(
        plain.starts_with(&format!("-i /p/in.heic -vf {scale} ")),
        "{plain}"
    );
    assert!(plain.contains("-pix_fmt rgb24"), "{plain}");
}

#[test]
fn heic_args_sips() {
    assert_eq!(
        heic_args(
            "sips",
            "{ format = 'jpeg', quality = 'small', side = '1080' }"
        ),
        "-s format jpeg -s formatOptions 70 -Z 1080 /p/in.heic --out /p/out.ext"
    );
    assert_eq!(
        heic_args("sips", "{ format = 'png', quality = 'best', side = 'src' }"),
        "-s format png /p/in.heic --out /p/out.ext"
    );
}

#[test]
fn heic_needs_ffmpeg_that_joins_tiles() {
    let code = bundled("io.github.leinnan.heic-convert");
    let vm = vm(&code, None);
    for (line, supported) in [
        ("ffmpeg version 8.1.1 Copyright (c) 2000-2026", true),
        ("ffmpeg version n9.0 Copyright", true),
        ("ffmpeg version 10.0-essentials_build-www.gyan.dev", true),
        ("ffmpeg version 8.0.1 Copyright", false),
        ("ffmpeg version 7.1 Copyright", false),
        ("ffmpeg version N-120000-gabcdef Copyright", true),
        ("ffmpeg version 2026-01-01-git-abcdef-full_build", true),
    ] {
        let result: bool = vm
            .lua
            .load(format!(
                "return require('lib.args').ffmpeg_supported([[{line}]])"
            ))
            .eval()
            .expect("checks");
        assert_eq!(result, supported, "{line}");
    }
}

/// Both actions are offered on photos, however the extension is written, and
/// on nothing else.
#[test]
fn heic_actions_target_photos() {
    let code = bundled("io.github.leinnan.heic-convert");
    let item = |path: &str| TargetInfo {
        path: path.into(),
        is_dir: false,
        size: 1,
        modified: 0,
    };
    assert_eq!(code.manifest.actions.len(), 2);
    for action in &code.manifest.actions {
        for photo in ["a/IMG_0001.HEIC", "a/b.heic", "a/c.HEIF"] {
            assert!(action.targets.matches(&[item(photo)], true), "{photo}");
        }
        assert!(!action.targets.matches(&[item("a/clip.mov")], true));
    }
}

/// The form is valid, and warns about metadata only where it is lost.
#[test]
fn heic_form_by_platform() {
    let code = bundled("io.github.leinnan.heic-convert");
    let vm = vm(&code, None);
    let context = CallContext {
        targets: vec![TargetInfo {
            path: "C:/photos/IMG_0001.HEIC".into(),
            is_dir: false,
            size: 2048,
            modified: 0,
        }],
        ..Default::default()
    };
    for (os, warns) in [("macos", false), ("windows", true), ("linux", true)] {
        vm.lua
            .load(format!("dirfleet.os = '{os}'"))
            .exec()
            .expect("sets os");
        let form: super::form::Form = mlua::LuaSerdeExt::from_value(
            &vm.lua,
            call(&vm, &["actions", "convert", "form"], &context),
        )
        .expect("valid form");
        form.check().expect("schema");
        let warning = form.all_fields().into_iter().any(|field| {
            field
                .text
                .as_deref()
                .is_some_and(|text| text.contains("capture date"))
        });
        assert_eq!(warning, warns, "{os}");
    }
}

fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Builds `ctx` and calls `M.<name>` with it.
fn call(vm: &Vm, name: &[&str], context: &CallContext) -> Value {
    let function = vm.handler(name).expect("handler");
    let ctx: Table = vm.context(context).expect("ctx");
    vm.call(&function, ctx, None).expect("handler runs")
}

#[test]
fn git_bar_and_inspector_in_a_repository() {
    let dir = temp_dir("git_plugin");
    if !git(&dir, &["init", "-q", "-b", "trunk"]) {
        eprintln!("git isn't available; skipping");
        return;
    }
    git(&dir, &["config", "user.email", "test@example.com"]);
    git(&dir, &["config", "user.name", "Test"]);
    let file = dir.join("notes.txt");
    std::fs::write(&file, "one").expect("write");
    let code = bundled("io.github.leinnan.git");
    assert!(code.manifest.activation.matches(&dir));
    let vm = vm(&code, None);
    let context = CallContext {
        dir: Some(dir.clone()),
        ..Default::default()
    };
    let bar: Vec<super::BarSegment> =
        mlua::LuaSerdeExt::from_value(&vm.lua, call(&vm, &["bar"], &context)).expect("segments");
    assert_eq!(bar[0].text.as_deref(), Some("trunk"));
    assert!(
        bar.iter().any(|s| s.badge.as_deref() == Some("1 changed")),
        "{bar:?}"
    );
    assert!(bar.iter().any(|s| s.action.as_deref() == Some("commit")));

    let inspector = CallContext {
        dir: Some(dir.clone()),
        targets: vec![target(&file)],
        ..Default::default()
    };
    let rows: Vec<super::InspectorRow> =
        mlua::LuaSerdeExt::from_value(&vm.lua, call(&vm, &["inspector"], &inspector))
            .expect("rows");
    assert_eq!(
        rows[0],
        super::InspectorRow::Pair("Status".into(), "Untracked".into())
    );

    // Commit through the action, as the form would submit it.
    let mut values = super::form::Values::new();
    values.insert("message".into(), "First".into());
    values.insert("all".into(), true.into());
    values.insert("push".into(), false.into());
    let commit = CallContext {
        values: Some(values),
        ..context.clone()
    };
    let run = vm.handler(&["actions", "commit", "run"]).expect("run");
    let ctx = vm.context(&commit).expect("ctx");
    let values = mlua::LuaSerdeExt::to_value(&vm.lua, commit.values.as_ref().expect("values"))
        .expect("values");
    let result: Value = vm.call(&run, (ctx, values), None).expect("commits");
    assert!(result.is_nil(), "{result:?}");
    let bar: Vec<super::BarSegment> =
        mlua::LuaSerdeExt::from_value(&vm.lua, call(&vm, &["bar"], &context)).expect("segments");
    assert!(
        bar.iter().any(|s| s.text.as_deref() == Some("clean")),
        "{bar:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bar_is_not_shown_outside_repositories() {
    let dir = temp_dir("not_git");
    let code = bundled("io.github.leinnan.git");
    if dir
        .ancestors()
        .any(|ancestor| ancestor.join(".git").exists())
    {
        return;
    }
    assert!(!code.manifest.activation.matches(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Converts a generated two-second MOV with the app's `FFmpeg`, whole and as
/// a fragment. Needs `FFmpeg` installed; run with `--ignored`.
#[test]
#[ignore = "needs FFmpeg"]
fn real_ffmpeg_converts_mov_to_mp4() {
    let (ffmpeg, _) = crate::app::assets::ffmpeg_tool_paths();
    assert!(ffmpeg.is_file(), "FFmpeg is not installed");
    let dir = temp_dir("video_convert");
    let source = dir.join("clip.mov");
    let made = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=320x240:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=duration=2",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&source)
        .status()
        .expect("ffmpeg runs");
    assert!(made.success());

    let code = bundled("io.github.leinnan.video-convert");
    let vm = vm(&code, Some(JobLink::new(1)));
    let item = target(&source);
    let context = CallContext {
        dir: Some(dir.clone()),
        targets: vec![item],
        ..Default::default()
    };
    let ctx = vm.context(&context).expect("ctx");
    let first: Table = ctx
        .get::<Table>("targets")
        .expect("targets")
        .get(1)
        .expect("item");
    let run_item = vm
        .handler(&["actions", "to_mp4", "run_item"])
        .expect("run_item");
    let result: Value = vm
        .call(&run_item, (ctx.clone(), first.clone()), None)
        .expect("converts");
    assert!(result.is_nil(), "{result:?}");
    let output = dir.join("clip.mp4");
    assert!(output.is_file());

    // The form for one video offers a fragment up to its length.
    let form = vm.handler(&["actions", "convert", "form"]).expect("form");
    let form: Value = vm.call(&form, ctx.clone(), None).expect("form builds");
    let form: super::form::Form = mlua::LuaSerdeExt::from_value(&vm.lua, form).expect("valid form");
    form.check().expect("schema");
    let finish = form
        .all_fields()
        .into_iter()
        .find(|field| field.id.as_deref() == Some("finish"))
        .and_then(|field| field.max)
        .expect("fragment end has the duration as max");
    assert!((finish - 2.0).abs() < 0.2, "{finish}");

    // Converting again keeps both files, and a fragment is shorter.
    let mut values = form.initial_values(None);
    values.insert("range".into(), "part".into());
    values.insert("start".into(), 0.5.into());
    values.insert("finish".into(), 1.5.into());
    let values = mlua::LuaSerdeExt::to_value(&vm.lua, &values).expect("values");
    let convert = vm
        .handler(&["actions", "convert", "run_item"])
        .expect("run_item");
    let result: Value = vm
        .call(&convert, (ctx, first, values), None)
        .expect("converts");
    assert!(result.is_nil(), "{result:?}");
    let fragment = dir.join("clip (2).mp4");
    let probe = vm
        .lua
        .load(format!(
            "return dirfleet.media.probe([[{}]]).duration",
            fragment.display()
        ))
        .eval::<f64>()
        .expect("probe");
    assert!((probe - 1.0).abs() < 0.3, "fragment is {probe}s");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The width `sips` reads from an image.
#[cfg(target_os = "macos")]
fn sips_width(path: &Path) -> u32 {
    let output = Command::new("sips")
        .args(["-g", "pixelWidth"])
        .arg(path)
        .output()
        .expect("sips runs");
    String::from_utf8_lossy(&output.stdout)
        .split("pixelWidth: ")
        .nth(1)
        .and_then(|width| width.trim().parse().ok())
        .unwrap_or_else(|| panic!("no width for {}", path.display()))
}

/// Runs `convert` on `source` with these form choices.
#[cfg(target_os = "macos")]
fn heic_convert(vm: &Vm, source: &Path, format: &str, side: &str, exists: &str) -> Value {
    let context = CallContext {
        targets: vec![target(source)],
        ..Default::default()
    };
    let ctx = vm.context(&context).expect("ctx");
    let item: Table = ctx
        .get::<Table>("targets")
        .expect("targets")
        .get(1)
        .expect("item");
    let form = vm.handler(&["actions", "convert", "form"]).expect("form");
    let form: Value = vm.call(&form, ctx.clone(), None).expect("form builds");
    let form: super::form::Form = mlua::LuaSerdeExt::from_value(&vm.lua, form).expect("valid form");
    let mut values = form.initial_values(None);
    values.insert("format".into(), format.into());
    values.insert("side".into(), side.into());
    values.insert("exists".into(), exists.into());
    let values = mlua::LuaSerdeExt::to_value(&vm.lua, &values).expect("values");
    let run = vm
        .handler(&["actions", "convert", "run_item"])
        .expect("run_item");
    vm.call(&run, (ctx, item, values), None).expect("runs")
}

/// Converts a tiled wallpaper and a small photo with `sips` and `FFmpeg`. Needs
/// macOS, for the photos, and `FFmpeg` 8.1; run with `--ignored`.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs macOS wallpapers and FFmpeg"]
fn real_heic_converts_with_both_backends() {
    let (ffmpeg, _) = crate::app::assets::ffmpeg_tool_paths();
    assert!(ffmpeg.is_file(), "FFmpeg is not installed");
    let wallpaper = Path::new("/System/Library/Desktop Pictures/iMac Blue.heic");
    assert!(wallpaper.is_file(), "no wallpaper to convert");

    for os in ["macos", "linux"] {
        let dir = temp_dir(&format!("heic_{os}"));
        // An uppercase extension, spaces and a '%' as cameras and people write them.
        let tiled = dir.join("Big 50%d photo.HEIC");
        std::fs::copy(wallpaper, &tiled).expect("copy");
        let png = dir.join("small.png");
        let made = Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=800x600",
                "-frames:v",
                "1",
                "-update",
                "1",
            ])
            .arg(&png)
            .status()
            .expect("ffmpeg runs");
        assert!(made.success());
        let small = dir.join("small.heic");
        let made = Command::new("sips")
            .args(["-s", "format", "heic"])
            .arg(&png)
            .arg("--out")
            .arg(&small)
            .output()
            .expect("sips runs");
        assert!(made.status.success());
        std::fs::remove_file(&png).expect("remove");

        let code = bundled("io.github.leinnan.heic-convert");
        let vm = vm(&code, Some(JobLink::new(1)));
        vm.lua
            .load(format!("dirfleet.os = '{os}'"))
            .exec()
            .expect("sets os");

        // One click: the whole photo as JPEG, not one tile.
        let context = CallContext {
            targets: vec![target(&tiled)],
            ..Default::default()
        };
        let ctx = vm.context(&context).expect("ctx");
        let item: Table = ctx
            .get::<Table>("targets")
            .expect("targets")
            .get(1)
            .expect("item");
        let run = vm
            .handler(&["actions", "to_jpeg", "run_item"])
            .expect("run_item");
        let result: Value = vm.call(&run, (ctx, item), None).expect("converts");
        assert!(result.is_nil(), "{os}: {result:?}");
        assert_eq!(sips_width(&dir.join("Big 50%d photo.jpg")), 6016, "{os}");

        // Each format, scaled down from the tile grid.
        for (format, ext) in [("jpeg", "jpg"), ("png", "png"), ("tiff", "tiff")] {
            let result = heic_convert(&vm, &tiled, format, "2048", "overwrite");
            assert!(result.is_nil(), "{os} {format}: {result:?}");
            let output = dir.join(format!("Big 50%d photo.{ext}"));
            assert_eq!(sips_width(&output), 2048, "{os} {format}");
        }

        // Not split into tiles, and never enlarged.
        let result = heic_convert(&vm, &small, "jpeg", "400", "rename");
        assert!(result.is_nil(), "{os}: {result:?}");
        assert_eq!(sips_width(&dir.join("small.jpg")), 400, "{os}");
        let result = heic_convert(&vm, &small, "jpeg", "4096", "overwrite");
        assert!(result.is_nil(), "{os}: {result:?}");
        assert_eq!(sips_width(&dir.join("small.jpg")), 800, "{os}");

        // What to do when the output exists.
        let result = heic_convert(&vm, &small, "jpeg", "src", "rename");
        assert!(result.is_nil(), "{os}: {result:?}");
        assert!(dir.join("small (2).jpg").is_file(), "{os}");
        let before = std::fs::read(dir.join("small.jpg")).expect("read");
        std::fs::write(dir.join("small.jpg"), b"keep").expect("write");
        let result = heic_convert(&vm, &small, "jpeg", "src", "skip");
        assert!(!result.is_nil(), "{os}: should report skipped");
        assert_eq!(std::fs::read(dir.join("small.jpg")).expect("read"), b"keep");
        assert!(!dir.join("small (3).jpg").exists(), "{os}");
        let result = heic_convert(&vm, &small, "jpeg", "src", "overwrite");
        assert!(result.is_nil(), "{os}: {result:?}");
        assert_eq!(std::fs::read(dir.join("small.jpg")).expect("read"), before);

        // A broken file is reported, and leaves nothing behind.
        let broken = dir.join("broken.heic");
        std::fs::write(&broken, b"not a photo").expect("write");
        let result = heic_convert(&vm, &broken, "jpeg", "src", "rename");
        let Value::Table(result) = result else {
            panic!("{os}: expected an error, got {result:?}");
        };
        let error: String = result.get("error").expect("an error message");
        assert!(!error.is_empty(), "{os}");
        assert!(!dir.join("broken.jpg").exists(), "{os}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A video without audio has working inspector rows and a working form.
/// Needs `FFmpeg` installed; run with `--ignored`.
#[test]
#[ignore = "needs FFmpeg"]
fn real_ffmpeg_silent_video_in_inspector_and_form() {
    let (ffmpeg, _) = crate::app::assets::ffmpeg_tool_paths();
    assert!(ffmpeg.is_file(), "FFmpeg is not installed");
    let dir = temp_dir("video_silent");
    let source = dir.join("silent.mov");
    let made = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=320x240:rate=25",
            "-c:v",
            "libx264",
        ])
        .arg(&source)
        .status()
        .expect("ffmpeg runs");
    assert!(made.success());

    let code = bundled("io.github.leinnan.video-convert");
    let vm = vm(&code, None);
    let context = CallContext {
        dir: Some(dir.clone()),
        targets: vec![target(&source)],
        ..Default::default()
    };
    let rows: Vec<super::InspectorRow> =
        mlua::LuaSerdeExt::from_value(&vm.lua, call(&vm, &["inspector"], &context)).expect("rows");
    assert!(
        rows.contains(&super::InspectorRow::Pair("Audio".into(), "None".into())),
        "{rows:?}"
    );
    let form: super::form::Form = mlua::LuaSerdeExt::from_value(
        &vm.lua,
        call(&vm, &["actions", "convert", "form"], &context),
    )
    .expect("valid form");
    form.check().expect("schema");
    let _ = std::fs::remove_dir_all(&dir);
}

fn snapshot_harness<'a>(
    size: egui::Vec2,
    mut draw: impl FnMut(&mut egui::Ui) + 'a,
) -> egui_kittest::Harness<'a> {
    egui_kittest::Harness::builder()
        .with_options(crate::app::ui::snapshot_options())
        .with_size(size)
        .build_ui(move |ui| {
            let ctx = ui.ctx().clone();
            if !ctx.data(|data| {
                data.get_temp::<bool>(egui::Id::new("fonts_set"))
                    .unwrap_or(false)
            }) {
                let mut fonts = egui::FontDefinitions::default();
                crate::app::ui::register_icons(&mut fonts);
                ctx.set_fonts(fonts);
                crate::app::ui::configure_test(&ctx, false);
                ctx.data_mut(|data| data.insert_temp(egui::Id::new("fonts_set"), true));
                return;
            }
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    draw(ui);
                });
        })
}

/// The directory bar, the video converter's form for one video and the
/// Plugins settings page.
#[test]
fn plugin_ui_snapshots() {
    let _guard = crate::app::dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut snapshots = egui_kittest::SnapshotResults::new();

    let segment = |json: serde_json::Value| -> super::BarSegment {
        serde_json::from_value(json).expect("segment")
    };
    let mut state = super::host::BarState::default();
    state.segments = vec![
        segment(serde_json::json!({ "text": "main", "icon": "git-branch" })),
        segment(serde_json::json!({ "text": "2", "icon": "arrow-up" })),
        segment(serde_json::json!({ "badge": "3 changed", "tone": "warn" })),
        segment(
            serde_json::json!({ "button": "Pull", "action": "pull", "icon": "arrow-down-to-line" }),
        ),
        segment(
            serde_json::json!({ "button": "Push", "action": "push", "icon": "arrow-up-from-line" }),
        ),
        segment(serde_json::json!({ "button": "Commit…", "action": "commit" })),
    ];
    let views = [super::host::BarView {
        plugin: "io.github.leinnan.git",
        name: "Git",
        state: &state,
    }];
    let mut harness = snapshot_harness(egui::vec2(720.0, 56.0), |ui| {
        super::ui::bar_ui(ui, &views);
    });
    harness.run_steps(4);
    snapshots.add(harness.try_snapshot("plugin_bar"));
    drop(harness);

    // The converter's form as its Lua builds it, for one video
    // (the file doesn't exist, so the summary says its details are unknown).
    let code = bundled("io.github.leinnan.video-convert");
    let vm = vm(&code, None);
    let form = vm.handler(&["actions", "convert", "form"]).expect("form");
    let context = CallContext {
        targets: vec![TargetInfo {
            path: "C:/videos/holiday.mov".into(),
            is_dir: false,
            size: 1,
            modified: 0,
        }],
        ..Default::default()
    };
    let form: Value = vm
        .call(&form, vm.context(&context).expect("ctx"), None)
        .expect("form");
    let form: super::form::Form = mlua::LuaSerdeExt::from_value(&vm.lua, form).expect("form");
    let mut state = super::form::FormState::new(form, None);
    state.values.insert("range".into(), "part".into());
    let mut harness = snapshot_harness(egui::vec2(520.0, 460.0), move |ui| {
        state.ui(ui, "Convert video…");
    });
    harness.run_steps(4);
    snapshots.add(harness.try_snapshot("plugin_convert_form"));
    drop(harness);

    // Git only: the converter's FFmpeg row depends on the machine.
    let mut host = super::PluginHost::with_plugins(vec![bundled("io.github.leinnan.git")]);
    let mut settings = super::host::SettingsMap::new();
    let mut harness = snapshot_harness(egui::vec2(640.0, 900.0), move |ui| {
        super::ui::settings_page(ui, &mut host, &mut settings);
    });
    harness.run_steps(4);
    snapshots.add(harness.try_snapshot("plugin_settings"));
    snapshots.unwrap();
}

const RUST_CLEAN: &str = "io.github.leinnan.rust-clean";
const FOLDER_USAGE: &str = "io.github.leinnan.folder-usage";

/// The cargo to build fixtures with, found the way the plugin finds it, so
/// these tests also run with `~/.cargo/bin` off the `PATH`.
#[cfg(unix)]
fn fixture_cargo() -> String {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".cargo/bin/cargo"))
        .filter(|path| path.is_file())
        .map_or_else(|| "cargo".to_owned(), |path| path.display().to_string())
}

/// A folder under the real temp dir, as cargo reports it (`/tmp` is a link
/// on macOS).
#[cfg(unix)]
fn rust_dir(name: &str) -> PathBuf {
    temp_dir(name).canonicalize().expect("canonical")
}

#[cfg(unix)]
fn write_package(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir.join("src")).expect("creates");
    std::fs::write(
        dir.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .expect("writes");
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").expect("writes");
}

#[cfg(unix)]
fn cargo_build(dir: &Path) {
    let output = Command::new(fixture_cargo())
        .args(["build", "--offline", "--quiet"])
        .current_dir(dir)
        .output()
        .expect("cargo runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `lib/cargo.lua`'s plan as `(paths, refusal, notes, cargo_clean)`.
#[cfg(unix)]
fn clean_plan(function: &str, root: &Path) -> (Vec<String>, Option<String>, Vec<String>, bool) {
    let code = bundled(RUST_CLEAN);
    let vm = vm(&code, None);
    let plan: Table = vm
        .lua
        .load(format!(
            "return require('lib.cargo').{function}([[{}]])",
            root.display()
        ))
        .eval()
        .expect("plan");
    (
        plan.get("paths").expect("paths"),
        plan.get("refusal").expect("refusal"),
        plan.get("notes").expect("notes"),
        plan.get("cargo_clean").expect("cargo_clean"),
    )
}

/// Runs `M.actions.<action>.run` in a project.
#[cfg(unix)]
fn clean_run(action: &str, root: &Path) -> Value {
    let code = bundled(RUST_CLEAN);
    let vm = vm(&code, None);
    let context = CallContext {
        dir: Some(root.to_path_buf()),
        ..Default::default()
    };
    call(&vm, &["actions", action, "run"], &context)
}

#[test]
fn rust_clean_reads_only_top_level_metadata() {
    let code = bundled(RUST_CLEAN);
    let vm = vm(&code, None);
    let json = r#" {"packages":[{"name":"a","target_directory":"/wrong/package","metadata":{"workspace_root":"/wrong"}}],
        "resolve":null,"version":1,"target_directory":"C:\\p \"x\"\/caf\u00e9",
        "workspace_root":"/w","metadata":{"build_directory":"/wrong/workspace","x":["}","{"]},"last":true} "#;
    let fields: std::collections::BTreeMap<String, String> = vm
        .lua
        .load(format!(
            "return require('lib.cargo').top_level_strings([==[{json}]==])"
        ))
        .eval::<Table>()
        .expect("fields")
        .pairs()
        .collect::<mlua::Result<_>>()
        .expect("strings");
    assert_eq!(
        fields.get("target_directory").map(String::as_str),
        Some("C:\\p \"x\"/café")
    );
    assert_eq!(fields.get("workspace_root").map(String::as_str), Some("/w"));
    assert_eq!(
        fields.get("build_directory"),
        None,
        "nested keys don't count"
    );
    assert_eq!(fields.len(), 2);
}

#[test]
fn rust_clean_is_inside() {
    let code = bundled(RUST_CLEAN);
    let vm = vm(&code, None);
    let inside = |path: &str, base: &str| -> bool {
        vm.lua
            .load(format!(
                "return require('lib.cargo').is_inside([[{path}]], [[{base}]])"
            ))
            .eval()
            .expect("checks")
    };
    assert!(inside("/w/target", "/w"));
    assert!(inside("/w", "/w/"));
    assert!(
        !inside("/work/target", "/w"),
        "a shared prefix isn't inside"
    );
    assert!(!inside("/elsewhere", "/w"));
}

/// Clearing incremental takes only the caches, whatever else is around, and
/// the project still builds.
#[cfg(unix)]
#[test]
fn rust_clean_incremental_finds_only_real_caches() {
    let dir = rust_dir("rust_clean_incremental");
    write_package(&dir, "fixture");
    cargo_build(&dir);
    let target = dir.join("target");
    let debug = target.join("debug/incremental");
    assert!(debug.is_dir(), "a debug build leaves incremental caches");

    // Another triple's cache, and things that must not be taken for one.
    let triple = target.join("wasm32-unknown-unknown/debug/incremental");
    std::fs::create_dir_all(&triple).expect("creates");
    std::fs::create_dir_all(target.join("doc/incremental")).expect("creates");
    std::fs::create_dir_all(target.join("debug/deps/incremental")).expect("creates");
    let sentinel = temp_dir("rust_clean_sentinel");
    std::fs::write(sentinel.join("keep"), "keep").expect("writes");
    std::fs::create_dir_all(target.join("release")).expect("creates");
    std::os::unix::fs::symlink(&sentinel, target.join("release/incremental")).expect("links");

    let (paths, refusal, notes, _) = clean_plan("plan_incremental", &dir);
    assert_eq!(refusal, None);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(
        paths,
        [debug.display().to_string(), triple.display().to_string()]
    );
    assert!(sentinel.join("keep").is_file());

    let cancel = std::sync::atomic::AtomicBool::new(false);
    let measure = |path: &Path| {
        super::usage::measure(path, &super::usage::Options::default(), &cancel)
            .expect("walks")
            .expect("not cancelled")
    };
    let preview: u64 = paths
        .iter()
        .map(|path| measure(Path::new(path)).freed)
        .sum();
    let before = measure(&target).bytes;

    // What FileOp::Delete does with the plan; the next build must not mind.
    for path in &paths {
        std::fs::remove_dir_all(path).expect("removes");
    }
    let dropped = before - measure(&target).bytes;
    let ratio = preview as f64 / dropped as f64;
    assert!(
        (0.99..=1.01).contains(&ratio),
        "the form promised {preview} bytes and {dropped} went"
    );
    assert!(target.join("debug/deps").is_dir());
    std::fs::remove_file(target.join("release/incremental")).expect("removes");
    cargo_build(&dir);
    assert!(debug.is_dir(), "the build made its cache again");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&sentinel);
}

#[cfg(unix)]
#[test]
fn rust_clean_target_runs_cargo_clean() {
    let dir = rust_dir("rust_clean_target");
    write_package(&dir, "fixture");
    cargo_build(&dir);
    let (paths, refusal, _, cargo_clean) = clean_plan("plan_target", &dir);
    assert_eq!(refusal, None);
    assert_eq!(paths, [dir.join("target").display().to_string()]);
    assert!(cargo_clean);

    let result = clean_run("clear_target", &dir);
    assert!(matches!(result, Value::Nil), "{result:?}");
    assert!(!dir.join("target").exists());
    assert!(dir.join("src/main.rs").is_file());
    assert!(dir.join("Cargo.toml").is_file());

    let (_, refusal, _, _) = clean_plan("plan_target", &dir);
    assert!(refusal.is_some_and(|text| text.contains("nothing to clear")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A member's target is the workspace's: the plan says so and clears that.
#[cfg(unix)]
#[test]
fn rust_clean_workspace_member_clears_the_root() {
    let dir = rust_dir("rust_clean_workspace");
    std::fs::write(
        dir.join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\"]\nresolver = \"2\"\n",
    )
    .expect("writes");
    let member = dir.join("member");
    write_package(&member, "member");
    cargo_build(&dir);
    assert!(!member.join("target").exists());

    let (paths, refusal, notes, cargo_clean) = clean_plan("plan_target", &member);
    assert_eq!(refusal, None);
    assert_eq!(paths, [dir.join("target").display().to_string()]);
    assert!(cargo_clean);
    assert!(
        notes.iter().any(|note| note.contains("workspace")),
        "{notes:?}"
    );

    let (paths, _, _, _) = clean_plan("plan_incremental", &member);
    assert_eq!(
        paths,
        [dir.join("target/debug/incremental").display().to_string()]
    );

    let result = clean_run("clear_target", &member);
    assert!(matches!(result, Value::Nil), "{result:?}");
    assert!(!dir.join("target").exists());
    assert!(member.join("src/main.rs").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

/// `cargo clean` would wipe a target folder other projects share.
#[cfg(unix)]
#[test]
fn rust_clean_refuses_a_shared_target() {
    let dir = rust_dir("rust_clean_shared");
    let shared = rust_dir("rust_clean_shared_target");
    write_package(&dir, "fixture");
    std::fs::create_dir_all(dir.join(".cargo")).expect("creates");
    std::fs::write(
        dir.join(".cargo/config.toml"),
        format!("[build]\ntarget-dir = \"{}\"\n", shared.display()),
    )
    .expect("writes");
    cargo_build(&dir);
    std::fs::write(shared.join("other_project_sentinel"), "keep").expect("writes");

    let (paths, refusal, _, _) = clean_plan("plan_target", &dir);
    assert!(paths.is_empty(), "{paths:?}");
    assert!(
        refusal
            .as_deref()
            .is_some_and(|text| text.contains("outside this project")),
        "{refusal:?}"
    );
    let result = clean_run("clear_target", &dir);
    assert!(
        matches!(result, Value::Table(_)),
        "reports an error: {result:?}"
    );
    assert!(shared.join("other_project_sentinel").is_file());
    assert!(shared.join("debug").is_dir());

    let (paths, refusal, notes, _) = clean_plan("plan_incremental", &dir);
    assert_eq!(refusal, None);
    assert_eq!(
        paths,
        [shared.join("debug/incremental").display().to_string()]
    );
    assert!(notes.iter().any(|note| note.contains("share")), "{notes:?}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&shared);
}

/// With a manifest cargo can't read, only a folder that is provably Cargo's
/// output may go.
#[cfg(unix)]
#[test]
fn rust_clean_broken_manifest_needs_cargo_markers() {
    let dir = rust_dir("rust_clean_broken");
    std::fs::write(dir.join("Cargo.toml"), "this is not toml [").expect("writes");
    let target = dir.join("target");
    std::fs::create_dir_all(target.join("debug/incremental")).expect("creates");
    std::fs::write(target.join("precious.txt"), "keep").expect("writes");

    for function in ["plan_target", "plan_incremental"] {
        let (paths, refusal, _, _) = clean_plan(function, &dir);
        assert!(paths.is_empty(), "{function}: {paths:?}");
        assert!(
            refusal
                .as_deref()
                .is_some_and(|text| text.contains("doesn't look like")),
            "{function}: {refusal:?}"
        );
    }
    let result = clean_run("clear_target", &dir);
    assert!(matches!(result, Value::Table(_)), "{result:?}");
    assert!(target.join("precious.txt").is_file());

    // Both markers are enough; older Cargo wrote only the second.
    for marker in ["CACHEDIR.TAG", ".rustc_info.json"] {
        std::fs::write(target.join(marker), "").expect("writes");
        let (paths, refusal, notes, cargo_clean) = clean_plan("plan_target", &dir);
        assert_eq!(refusal, None, "{marker}");
        assert_eq!(paths, [target.display().to_string()], "{marker}");
        assert!(!cargo_clean, "cargo can't clean what it can't read");
        assert!(
            notes.iter().any(|note| note.contains("couldn't read")),
            "{notes:?}"
        );
        let (paths, _, _, _) = clean_plan("plan_incremental", &dir);
        assert_eq!(
            paths,
            [target.join("debug/incremental").display().to_string()]
        );
        std::fs::remove_file(target.join(marker)).expect("removes");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn rust_clean_forms_show_size_or_reason() {
    let dir = rust_dir("rust_clean_form");
    write_package(&dir, "fixture");
    let code = bundled(RUST_CLEAN);
    let vm = vm(&code, None);
    let context = CallContext {
        dir: Some(dir.clone()),
        ..Default::default()
    };
    let validate = |action: &str| -> Value {
        let function = vm
            .handler(&["actions", action, "validate"])
            .expect("validate");
        vm.call(&function, vm.context(&context).expect("ctx"), None)
            .expect("runs")
    };
    let submit = |action: &str| -> Option<String> {
        let form: Table = match call(&vm, &["actions", action, "form"], &context) {
            Value::Table(form) => form,
            other => panic!("{other:?}"),
        };
        form.get("submit").expect("submit")
    };
    // Nothing built yet: no button to go ahead, and validate refuses too.
    assert_eq!(submit("clear_target"), None);
    assert!(matches!(validate("clear_target"), Value::String(_)));
    assert!(matches!(validate("clear_incremental"), Value::String(_)));

    cargo_build(&dir);
    assert!(submit("clear_incremental").is_some_and(|text| text.starts_with("Delete ")));
    assert!(submit("clear_target").is_some_and(|text| text.starts_with("Delete ")));
    assert!(matches!(validate("clear_target"), Value::Nil));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn folder_usage_table_rows() {
    let dir = rust_dir("folder_usage");
    // A project one level down with a workspace member, and a plain folder.
    let project = dir.join("GAMES/rpg");
    std::fs::create_dir_all(project.join("member/src")).expect("creates");
    std::fs::write(project.join("Cargo.toml"), "").expect("writes");
    std::fs::write(project.join("member/Cargo.toml"), "").expect("writes");
    std::fs::create_dir_all(project.join("target/debug")).expect("creates");
    std::fs::write(project.join("target/CACHEDIR.TAG"), "").expect("writes");
    std::fs::write(project.join("target/debug/app"), vec![1u8; 1 << 20]).expect("writes");
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    let source = std::fs::File::create(project.join("main.rs")).expect("creates");
    source.set_modified(old).expect("sets the time");
    std::fs::create_dir_all(dir.join("notes")).expect("creates");
    std::fs::write(dir.join("notes/plan.txt"), "text").expect("writes");
    std::fs::create_dir_all(dir.join("node_modules/dep")).expect("creates");
    std::fs::write(dir.join("node_modules/dep/Cargo.toml"), "").expect("writes");

    let code = bundled(FOLDER_USAGE);
    let vm = vm(&code, None);
    // Sees what the plugin passes; the real function still checks it.
    vm.lua
        .load(
            "local real = dirfleet.app.show_table
             dirfleet.app.show_table = function(spec) shown = spec; real(spec) end",
        )
        .exec()
        .expect("wraps");
    let run = |show: &str| -> Table {
        let function = vm.handler(&["actions", "scan", "run"]).expect("run");
        let context = CallContext {
            dir: Some(dir.clone()),
            ..Default::default()
        };
        let values = vm.lua.create_table().expect("table");
        values.set("show", show).expect("sets");
        let result: Value = vm
            .call(
                &function,
                (vm.context(&context).expect("ctx"), values),
                None,
            )
            .expect("runs");
        assert!(matches!(result, Value::Nil), "{result:?}");
        vm.lua.globals().get("shown").expect("shown")
    };

    let rows: Vec<Table> = run("rust").get("rows").expect("rows");
    assert_eq!(
        rows.len(),
        1,
        "a member isn't a row; node_modules is skipped"
    );
    let cells: Table = rows[0].get("cells").expect("cells");
    assert_eq!(cells.get::<String>("name").expect("name"), "GAMES/rpg");
    assert!(cells.get::<i64>("target").expect("target") >= 1 << 20);
    assert!(cells.get::<i64>("size").expect("size") >= cells.get::<i64>("target").expect("target"));
    // The rest of the project is newer than this, but the build isn't counted.
    assert!(cells.get::<i64>("modified").expect("modified") > 1_000_000);

    let table = run("subfolders");
    let rows: Vec<Table> = table.get("rows").expect("rows");
    let names: Vec<String> = rows
        .iter()
        .map(|row| {
            row.get::<Table>("cells")
                .and_then(|cells| cells.get("name"))
                .expect("name")
        })
        .collect();
    assert_eq!(names, ["GAMES", "node_modules", "notes"]);
    let notes: Table = rows[2].get("cells").expect("cells");
    assert_eq!(notes.get::<Value>("target").expect("target"), Value::Nil);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(not(windows))]
#[test]
fn plugin_table_snapshot() {
    let _guard = crate::app::dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let spec: super::table::TableSpec = serde_json::from_value(serde_json::json!({
        "title": "Folder Usage: Documents",
        "subtitle": "3 projects, 201.0 GB",
        "columns": [
            { "id": "name", "label": "Folder" },
            { "id": "size", "label": "Size", "kind": "size" },
            { "id": "target", "label": "target", "kind": "size" },
            { "id": "modified", "label": "Last change", "kind": "time" },
        ],
        "rows": [
            { "path": "/d/lwa_fm", "cells": { "name": "APPS/lwa_fm", "size": 31_000_000_000_i64, "target": 23_000_000_000_i64, "modified": 1_793_000_000 } },
            { "path": "/d/rpg", "cells": { "name": "GAMES/IdleRPG", "size": 151_000_000_000_i64, "target": 148_000_000_000_i64, "modified": 1_773_000_000 } },
            { "path": "/d/notes", "cells": { "name": "NOTES", "size": 19_000_000, "modified": 1_754_000_000 } },
        ],
        "sort": { "column": "size", "descending": true },
    }))
    .expect("spec");
    let mut table = super::table::TableView::new(spec);
    let mut harness = snapshot_harness(egui::vec2(640.0, 180.0), move |ui| {
        table.contents_ui(ui);
    });
    harness.run_steps(4);
    harness.snapshot("plugin_table");
}

/// `fs.usage` against `du -sk`, on the folders in `DIRFLEET_USAGE_PATHS`
/// (separated by `:`). Read-only. Nothing may be building in them.
#[cfg(unix)]
#[test]
#[ignore = "needs DIRFLEET_USAGE_PATHS"]
fn real_usage_matches_du() {
    let paths = std::env::var("DIRFLEET_USAGE_PATHS").expect("DIRFLEET_USAGE_PATHS");
    for path in paths.split(':') {
        let usage = super::usage::measure(
            Path::new(path),
            &super::usage::Options::default(),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect("walks")
        .expect("not cancelled");
        let output = Command::new("du")
            .args(["-sk", path])
            .output()
            .expect("du runs");
        let du: f64 = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .and_then(|kb| kb.parse::<f64>().ok())
            .expect("du's number")
            * 1024.0;
        let ratio = usage.bytes as f64 / du;
        println!(
            "{path}: usage {} freed {} du {du} ratio {ratio:.5} errors {}",
            usage.bytes, usage.freed, usage.errors
        );
        assert!((0.99..=1.01).contains(&ratio), "{path}: {ratio}");
    }
}

/// Folder Usage on a real folder, printing its rows to compare with `du` and
/// `git log` by hand. Read-only.
#[test]
#[ignore = "needs DIRFLEET_SCAN_DIR"]
fn real_folder_usage_rows() {
    let dir = PathBuf::from(std::env::var("DIRFLEET_SCAN_DIR").expect("DIRFLEET_SCAN_DIR"));
    let code = bundled(FOLDER_USAGE);
    let vm = vm(&code, None);
    let started = std::time::Instant::now();
    let stamp = vm
        .lua
        .create_function(move |_, ()| {
            println!("FIRST ROWS {:.2?}", started.elapsed());
            Ok(())
        })
        .expect("function");
    vm.lua.globals().set("first_rows", stamp).expect("sets");
    vm.lua
        .load(
            "local real = dirfleet.app.show_table
             dirfleet.app.show_table = function(spec)
                 if not shown then first_rows() end
                 shown = spec
                 real(spec)
             end",
        )
        .exec()
        .expect("wraps");
    let function = vm.handler(&["actions", "scan", "run"]).expect("run");
    let context = CallContext {
        dir: Some(dir),
        ..Default::default()
    };
    let values = vm.lua.create_table().expect("table");
    values.set("show", "rust").expect("sets");
    let _: Value = vm
        .call(
            &function,
            (vm.context(&context).expect("ctx"), values),
            None,
        )
        .expect("runs");
    let shown: Table = vm.lua.globals().get("shown").expect("shown");
    println!("FINISHED {:.2?}", started.elapsed());
    println!("{}", shown.get::<String>("subtitle").expect("subtitle"));
    let rows: Vec<Table> = shown.get("rows").expect("rows");
    let mut lines: Vec<(i64, String)> = rows
        .iter()
        .map(|row| {
            let cells: Table = row.get("cells").expect("cells");
            let size: i64 = cells.get("size").expect("size");
            let target: Option<i64> = cells.get("target").expect("target");
            let modified: Option<i64> = cells.get("modified").expect("modified");
            let name: String = cells.get("name").expect("name");
            (size, format!("ROW {size} {target:?} {modified:?} {name}"))
        })
        .collect();
    lines.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
    for (_, line) in lines {
        println!("{line}");
    }
}

/// Cancel during a scan of `DIRFLEET_SCAN_DIR`: the walks stop quickly, and
/// returning at all means their threads ended. Read-only.
#[test]
#[ignore = "needs DIRFLEET_SCAN_DIR"]
fn real_scan_cancels_quickly() {
    let dir = PathBuf::from(std::env::var("DIRFLEET_SCAN_DIR").expect("DIRFLEET_SCAN_DIR"));
    let code = bundled(FOLDER_USAGE);
    let vm = vm(&code, None);
    let cancel = Arc::clone(&vm.cancel);
    let asked = Arc::new(std::sync::Mutex::new(None));
    let asked_at = Arc::clone(&asked);
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(6));
        *asked_at.lock().expect("lock") = Some(std::time::Instant::now());
        cancel.store(true, std::sync::atomic::Ordering::Release);
    });
    let function = vm.handler(&["actions", "scan", "run"]).expect("run");
    let context = CallContext {
        dir: Some(dir),
        ..Default::default()
    };
    let values = vm.lua.create_table().expect("table");
    values.set("show", "rust").expect("sets");
    let result = vm.call::<_, Value>(
        &function,
        (vm.context(&context).expect("ctx"), values),
        None,
    );
    canceller.join().expect("joins");
    let waited = asked.lock().expect("lock").expect("asked").elapsed();
    println!("CANCEL took {waited:.2?}: {result:?}");
    assert!(waited < std::time::Duration::from_secs(2), "{waited:?}");
}

/// Results need somewhere to go, so a plugin can't send them without the dialog.
#[test]
fn show_table_needs_a_results_action() {
    let code = bundled("io.github.leinnan.git");
    let vm = vm(&code, None);
    let error = vm
        .lua
        .load("dirfleet.app.show_table({ title = 'T', columns = { { id = 'a', label = 'A' } } })")
        .exec()
        .expect_err("refused");
    assert!(error.to_string().contains("results = true"), "{error}");
}

/// Removal is the Trash unless asked otherwise, and never the home folder.
#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn remove_folder_trashes_by_default_and_refuses_home() {
    let dir = rust_dir("remove_folder");
    std::fs::create_dir_all(dir.join(".git")).expect("creates");
    std::fs::write(dir.join("file.txt"), "text").expect("writes");
    let code = bundled(FOLDER_USAGE);
    let vm = vm(&code, None);
    // Sees the requests instead of carrying them out.
    vm.lua
        .load(
            "asked = {}
             dirfleet.ops.trash = function(path) asked[#asked + 1] = 'trash ' .. path end
             dirfleet.ops.delete = function(path) asked[#asked + 1] = 'delete ' .. path end",
        )
        .exec()
        .expect("wraps");
    let context = |path: &Path| CallContext {
        dir: Some(path.to_path_buf()),
        ..Default::default()
    };
    let run = |path: &Path, how: &str| -> Value {
        let function = vm
            .handler(&["actions", "remove_folder", "run"])
            .expect("run");
        let values = vm.lua.create_table().expect("table");
        values.set("how", how).expect("sets");
        vm.call(
            &function,
            (vm.context(&context(path)).expect("ctx"), values),
            None,
        )
        .expect("runs")
    };

    let form = call(&vm, &["actions", "remove_folder", "form"], &context(&dir));
    let Value::Table(form) = form else {
        panic!("{form:?}")
    };
    let fields: Vec<Table> = form.get("fields").expect("fields");
    let text: Vec<String> = fields
        .iter()
        .filter_map(|field| field.get::<Option<String>>("text").expect("text"))
        .collect();
    let paths = fields
        .iter()
        .find_map(|field| field.get::<Option<Vec<Table>>>("rows").expect("rows"))
        .expect("a list of paths");
    assert_eq!(
        paths[0].get::<String>("path").expect("path"),
        dir.display().to_string(),
        "says which folder"
    );
    assert!(text.iter().any(|line| line.contains("git")), "{text:?}");
    let how = fields.last().expect("last field");
    assert_eq!(how.get::<String>("default").expect("default"), "trash");
    assert_eq!(
        form.get::<String>("submit").expect("submit"),
        "Move to Trash"
    );

    assert!(matches!(run(&dir, "trash"), Value::Nil));
    assert!(matches!(run(&dir, "delete"), Value::Nil));
    let asked: Vec<String> = vm.lua.globals().get("asked").expect("asked");
    let shown = dir.display();
    assert_eq!(asked, [format!("trash {shown}"), format!("delete {shown}")]);

    // Refused by the form, by validate and by run.
    let home = directories::BaseDirs::new()
        .expect("home")
        .home_dir()
        .to_path_buf();
    for refused in [
        PathBuf::from("/"),
        home.clone(),
        home.parent().expect("parent").to_path_buf(),
    ] {
        let validate = vm
            .handler(&["actions", "remove_folder", "validate"])
            .expect("validate");
        let reason: Value = vm
            .call(
                &validate,
                vm.context(&context(&refused)).expect("ctx"),
                None,
            )
            .expect("runs");
        assert!(matches!(reason, Value::String(_)), "{refused:?}");
        assert!(
            matches!(run(&refused, "delete"), Value::Table(_)),
            "{refused:?}"
        );
        let form = call(
            &vm,
            &["actions", "remove_folder", "form"],
            &context(&refused),
        );
        let Value::Table(form) = form else {
            panic!("{form:?}")
        };
        assert!(form.get::<bool>("blocked").expect("blocked"), "{refused:?}");
    }
    let asked: Vec<String> = vm.lua.globals().get("asked").expect("asked");
    assert_eq!(asked.len(), 2, "nothing more was asked for");

    // A sibling whose name starts the same isn't the home folder.
    let scan_refuses = |path: &str| -> bool {
        vm.lua
            .load(format!(
                "return require('lib.scan').removal_refusal([[{path}]]) ~= nil"
            ))
            .eval()
            .expect("checks")
    };
    assert!(!scan_refuses(&format!("{}x", home.display())));
    assert!(!scan_refuses(&format!("{}/Documents", home.display())));
    assert!(scan_refuses(&format!("{}/", home.display())));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The confirmation before a folder is removed.
#[cfg(not(windows))]
#[test]
fn plugin_remove_confirmation_snapshot() {
    let _guard = crate::app::dock::tests::SNAPSHOT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let form: super::form::Form = serde_json::from_value(serde_json::json!({
        "title": "Remove slavic_castles?",
        "submit": "Move to Trash",
        "submit_when": [
            { "when": { "field": "how", "equals": "delete" }, "label": "Delete 341 GB", "danger": true },
        ],
        "fields": [
            { "type": "info", "text": "This whole folder and everything in it:" },
            { "type": "paths", "rows": [{ "path": "/Users/test/Documents/GAMES/slavic_castles", "size": "341 GB" }] },
            { "type": "info", "text": "It has a git repository: unpushed commits and uncommitted changes go with it." },
            { "id": "how", "type": "radio", "label": "How",
              "options": [["trash", "Move to Trash"], ["delete", "Delete permanently"]], "default": "delete" },
        ],
    }))
    .expect("form");
    let mut state = super::form::FormState::new(form, None);
    let mut harness = snapshot_harness(egui::vec2(520.0, 260.0), move |ui| {
        state.ui(ui, "Remove");
    });
    harness.run_steps(4);
    harness.snapshot("plugin_remove_confirmation");
}
