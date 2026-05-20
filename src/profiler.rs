use std::time::Instant;

pub fn enable_profiling() {
    puffin::set_scopes_on(true);
}

pub fn start_puffin_server() {
    puffin::set_scopes_on(true);

    match puffin_http::Server::new("127.0.0.1:8585") {
        Ok(puffin_server) => {
            log::info!("Run:  cargo install puffin_viewer && puffin_viewer --url 127.0.0.1:8585");

            std::process::Command::new("puffin_viewer")
                .arg("--url")
                .arg("127.0.0.1:8585")
                .spawn()
                .ok();

            #[expect(clippy::mem_forget)]
            std::mem::forget(puffin_server);
        }
        Err(err) => {
            log::error!("Failed to start puffin server: {err}");
        }
    }
}

pub fn profiler_window(ctx: &egui::Context, visible: &mut bool) {
    if *visible {
        *visible = puffin_egui::profiler_window(ctx);
    }
}

pub struct FrameTracker {
    frame_index: u64,
    frame_start: Instant,
    prev_app_time_ms: f32,
    gpu_busy_count: u32,
}

impl Default for FrameTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameTracker {
    pub fn new() -> Self {
        Self {
            frame_index: 0,
            frame_start: Instant::now(),
            prev_app_time_ms: 0.0,
            gpu_busy_count: 0,
        }
    }

    pub fn begin_frame(&mut self) {
        let prev_total_ms = self.frame_start.elapsed().as_secs_f32() * 1000.0;
        let prev_render_ms = prev_total_ms - self.prev_app_time_ms;

        puffin::profile_scope!("lwa_fm::frame::metrics",
            &format!("frame={} total={:.0}ms app={:.0}ms render={:.0}ms depth={}",
                self.frame_index,
                prev_total_ms,
                self.prev_app_time_ms,
                prev_render_ms.max(0.0),
                self.gpu_busy_count));

        self.frame_start = Instant::now();
    }

    pub fn end_frame(&mut self) {
        self.prev_app_time_ms = self.frame_start.elapsed().as_secs_f32() * 1000.0;
        self.frame_index += 1;
    }

    pub fn report_gpu_status(&mut self, device: &wgpu::Device) {
        match device.poll(wgpu::PollType::Poll) {
            Ok(wgpu::PollStatus::QueueEmpty) => {
                self.gpu_busy_count = 0;
                puffin::profile_scope!("lwa_fm::gpu::status", "IDLE");
            }
            Ok(wgpu::PollStatus::Poll | wgpu::PollStatus::WaitSucceeded) => {
                let prev = self.gpu_busy_count;
                self.gpu_busy_count = prev.saturating_add(1);
                let label = if prev > 2 { "BACKLOG" } else { "BUSY" };
                puffin::profile_scope!("lwa_fm::gpu::status", label);
            }
            Err(_) => {
                puffin::profile_scope!("lwa_fm::gpu::status", "ERROR");
            }
        }
    }
}
