//! 入口：装配。
//!
//! 这里只做三件事——找到数据目录、打开数据库、把窗口跑起来。
//! 它不认识 SQL，也不认识界面细节；出错了用 anyhow 报一句人话就退出。

use ToDay::boot;
use ToDay::ui::theme;
use ToDay::window_state;
use anyhow::Context as _;
use eframe::egui;
use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetupCreateNew, wgpu};

fn main() -> anyhow::Result<()> {
    let path = ToDay::paths::database_path().context("找不到应用数据目录")?;
    let app = boot::assemble(&path).with_context(|| format!("打不开数据库 {}", path.display()))?;

    // 上一次的窗口尺寸；文件坏了就按默认来
    let window_path = ToDay::paths::window_state_path().context("找不到应用数据目录")?;
    let window = window_state::load(&window_path).unwrap_or_default();
    let app = app.with_window_memory(window_path);

    // 适配器偏好默认低功耗：日记本不该把独显叫醒。`WGPU_POWER_PREF=high` 仍可覆盖
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    setup.power_preference =
        wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::LowPower);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([window.size.0, window.size.1])
            .with_min_inner_size([window_state::MIN_SIZE.0, window_state::MIN_SIZE.1])
            .with_maximized(window.maximized)
            .with_title("ToDay"),
        // 关掉 MSAA：圆角与细线由 `epaint` 自己做 1px feathering，
        // 4× 只会让 wgpu 多建一个 4 倍大的采样目标、每帧多一次 resolve
        multisampling: 0,
        wgpu_options: WgpuConfiguration {
            wgpu_setup: setup.into(),
            ..Default::default()
        },
        ..Default::default()
    };

    eframe::run_native(
        "ToDay",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow::anyhow!("窗口起不来：{err}"))
}
