//! 入口：装配。
//!
//! 这里只做三件事——找到数据目录、打开数据库、把窗口跑起来。
//! 它不认识 SQL，也不认识界面细节；出错了用 anyhow 报一句人话就退出。

use ToDay::app::App;
use ToDay::store::sqlite::SqliteVault;
use ToDay::ui::theme;
use anyhow::Context as _;
use eframe::egui;

fn main() -> anyhow::Result<()> {
    let path = ToDay::paths::database_path().context("找不到应用数据目录")?;
    let vault =
        SqliteVault::open(&path).with_context(|| format!("打不开数据库 {}", path.display()))?;
    let app = App::new(Box::new(vault)).context("日记本初始化失败")?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 660.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("ToDay"),
        multisampling: 4,
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
