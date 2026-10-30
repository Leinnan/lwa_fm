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
    assert!(args.starts_with("-ss 1.500 -to 4.000 -i /v/in.mov"), "{args}");
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
    assert!(gif.contains("palettegen") && gif.contains("scale=-2:480") && gif.contains("-an"), "{gif}");
    let mp3 = ffmpeg_args("{ format = 'mp3' }").join(" ");
    assert!(mp3.contains("-vn -c:a libmp3lame -q:a 2"), "{mp3}");
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
    let bar: Vec<super::BarSegment> = mlua::LuaSerdeExt::from_value(&vm.lua, call(&vm, &["bar"], &context))
        .expect("segments");
    assert_eq!(bar[0].text.as_deref(), Some("trunk"));
    assert!(bar.iter().any(|s| s.badge.as_deref() == Some("1 changed")), "{bar:?}");
    assert!(bar.iter().any(|s| s.action.as_deref() == Some("commit")));

    let inspector = CallContext {
        dir: Some(dir.clone()),
        targets: vec![target(&file)],
        ..Default::default()
    };
    let rows: Vec<super::InspectorRow> =
        mlua::LuaSerdeExt::from_value(&vm.lua, call(&vm, &["inspector"], &inspector)).expect("rows");
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
    let bar: Vec<super::BarSegment> = mlua::LuaSerdeExt::from_value(&vm.lua, call(&vm, &["bar"], &context))
        .expect("segments");
    assert!(bar.iter().any(|s| s.text.as_deref() == Some("clean")), "{bar:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bar_is_not_shown_outside_repositories() {
    let dir = temp_dir("not_git");
    let code = bundled("io.github.leinnan.git");
    if dir.ancestors().any(|ancestor| ancestor.join(".git").exists()) {
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
            "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
            "testsrc=duration=2:size=320x240:rate=25", "-f", "lavfi", "-i",
            "sine=duration=2", "-c:v", "libx264", "-c:a", "aac", "-shortest",
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
    let first: Table = ctx.get::<Table>("targets").expect("targets").get(1).expect("item");
    let run_item = vm.handler(&["actions", "to_mp4", "run_item"]).expect("run_item");
    let result: Value = vm.call(&run_item, (ctx.clone(), first.clone()), None).expect("converts");
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
    let convert = vm.handler(&["actions", "convert", "run_item"]).expect("run_item");
    let result: Value = vm.call(&convert, (ctx, first, values), None).expect("converts");
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

fn snapshot_harness<'a>(size: egui::Vec2, mut draw: impl FnMut(&mut egui::Ui) + 'a) -> egui_kittest::Harness<'a> {
    egui_kittest::Harness::builder()
        .with_options(crate::app::ui::snapshot_options())
        .with_size(size)
        .build_ui(move |ui| {
            let ctx = ui.ctx().clone();
            if !ctx.data(|data| data.get_temp::<bool>(egui::Id::new("fonts_set")).unwrap_or(false)) {
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
            segment(serde_json::json!({ "button": "Pull", "action": "pull", "icon": "arrow-down-to-line" })),
            segment(serde_json::json!({ "button": "Push", "action": "push", "icon": "arrow-up-from-line" })),
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
    let form: Value = vm.call(&form, vm.context(&context).expect("ctx"), None).expect("form");
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
