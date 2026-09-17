//! 窗口几何的记忆：只有尺寸与是否最大化。
//!
//! 不认识 egui，也不认识窗口系统：就是一行文本的读写（`宽 高 最大化(0/1)`）。
//! 不记位置——换显示器或拔掉外接屏之后，记下的坐标可能落在看不见的地方。
//! 文件里没有任何正文，锁定状态下关窗也会记。

use std::io;
use std::path::Path;

/// 首次启动（或记忆坏掉）时用的窗口尺寸，与 `main` 的 `with_inner_size` 共用。
pub const DEFAULT_SIZE: (f32, f32) = (1000.0, 660.0);

/// 允许的最小窗口尺寸，与 `main` 的 `with_min_inner_size` 共用。
pub const MIN_SIZE: (f32, f32) = (720.0, 480.0);

/// 记忆尺寸的上限：比这更大的多半是坏文件或异常的屏幕
const MAX_SIDE: f32 = 16384.0;

/// 上一次的窗口状态
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowState {
    /// 普通（非最大化）状态下的内容区尺寸
    pub size: (f32, f32),
    /// 关窗时是不是最大化
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            size: DEFAULT_SIZE,
            maximized: false,
        }
    }
}

/// 这份尺寸能不能直接拿来开窗：有限，且在 [`MIN_SIZE`] 与 [`MAX_SIDE`] 之间。
fn usable(size: (f32, f32)) -> bool {
    let (width, height) = size;
    width.is_finite()
        && height.is_finite()
        && width >= MIN_SIZE.0
        && height >= MIN_SIZE.1
        && width <= MAX_SIDE
        && height <= MAX_SIDE
}

/// 读上一次的窗口状态。文件缺失、字段不足、数字读不出来、尺寸越界都返回 `None`，
/// 由调用方退回 [`WindowState::default`]——宁可回到默认尺寸，也不要拿 0×0 去开窗。
pub fn load(path: &Path) -> Option<WindowState> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut fields = text.split_whitespace();
    let width: f32 = fields.next()?.parse().ok()?;
    let height: f32 = fields.next()?.parse().ok()?;
    let maximized = match fields.next()? {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    if fields.next().is_some() {
        return None;
    }
    let size = (width, height);
    usable(size).then_some(WindowState { size, maximized })
}

/// 记下这次的窗口状态。写不进去（目录不存在、文件只读）返回 `Err`，由调用方决定怎么报。
///
/// `size = None` 表示这次没量到普通尺寸（窗口正最大化）：保留文件里上一次记下的普通尺寸，
/// 否则下次从最大化还原会得到一个和屏幕一样大的窗口。记忆坏掉时按 [`WindowState::default`] 兜底。
pub fn remember(path: &Path, size: Option<(f32, f32)>, maximized: bool) -> io::Result<()> {
    let previous = load(path).unwrap_or_default();
    let size = match size {
        Some(size) if usable(size) => size,
        _ => previous.size,
    };
    std::fs::write(
        path,
        format!("{} {} {}\n", size.0, size.1, u8::from(maximized)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        (dir, path)
    }

    #[test]
    fn a_remembered_state_comes_back() {
        let (_dir, path) = temp_path("window.txt");

        remember(&path, Some((1200.0, 800.0)), true).unwrap();
        assert_eq!(
            load(&path),
            Some(WindowState {
                size: (1200.0, 800.0),
                maximized: true,
            })
        );

        remember(&path, Some((900.0, 600.0)), false).unwrap();
        assert_eq!(
            load(&path),
            Some(WindowState {
                size: (900.0, 600.0),
                maximized: false,
            })
        );
    }

    #[test]
    fn a_broken_file_falls_back_to_nothing() {
        let (_dir, path) = temp_path("window.txt");

        std::fs::write(&path, "").unwrap();
        assert_eq!(load(&path), None, "空文件");

        std::fs::write(&path, "1000 660").unwrap();
        assert_eq!(load(&path), None, "少一个字段");

        std::fs::write(&path, "abc 660 0").unwrap();
        assert_eq!(load(&path), None, "宽不是数字");

        std::fs::write(&path, "1000 660 2").unwrap();
        assert_eq!(load(&path), None, "最大化标志只认 0 / 1");

        std::fs::write(&path, "1000 660 0\n1000 660 1\n").unwrap();
        assert_eq!(load(&path), None, "多出来的行不该被忽略");

        assert_eq!(load(&path.with_file_name("没有这个文件")), None);
    }

    #[test]
    fn a_size_off_the_rails_is_not_used_to_open_a_window() {
        let (_dir, path) = temp_path("window.txt");

        std::fs::write(&path, "0 0 0").unwrap();
        assert_eq!(load(&path), None, "0×0 的窗口没人能操作");

        std::fs::write(&path, "50000 50000 0").unwrap();
        assert_eq!(load(&path), None, "比任何屏幕都大");

        std::fs::write(&path, "719 700 0").unwrap();
        assert_eq!(load(&path), None, "比最小宽度还窄");
    }

    #[test]
    fn a_maximized_close_keeps_the_last_normal_size() {
        let (_dir, path) = temp_path("window.txt");
        remember(&path, Some((1200.0, 800.0)), false).unwrap();

        // 最大化关窗：只翻标志，尺寸还是上一次的普通尺寸
        remember(&path, None, true).unwrap();
        assert_eq!(
            load(&path),
            Some(WindowState {
                size: (1200.0, 800.0),
                maximized: true,
            })
        );

        // 没有上一次的普通尺寸时退回默认尺寸
        let (_dir2, fresh) = temp_path("window.txt");
        remember(&fresh, None, true).unwrap();
        assert_eq!(
            load(&fresh),
            Some(WindowState {
                size: DEFAULT_SIZE,
                maximized: true,
            })
        );
    }

    #[test]
    fn a_missing_directory_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("没有这个目录").join("window.txt");
        assert!(remember(&path, Some((1000.0, 660.0)), false).is_err());
    }
}
