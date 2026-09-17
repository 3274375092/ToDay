//! 解锁界面：第一次设主密码，之后每次启动输密码进来。
//!
//! 密码只在视图里活着：缓冲区用 [`Zeroizing`]，清空时整块擦掉，drop 时也擦。
//! 提交之后由控制器去问存储层，失败原因再作为 `external_error` 回到这里显示。

use crate::ui::{shell, theme};
use egui::{Align, CornerRadius, Layout, Margin, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};
use zeroize::{Zeroize, Zeroizing};

/// 这个卡片是来干什么的
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnlockMode {
    /// 首次启动：设一个主密码，输两遍
    Setup,
    /// 之后每次启动：输一遍
    Unlock,
}

/// 主密码的下限。太短的密码挡不住一次词典攻击，Argon2 也救不回来。
const MIN_PASSWORD_CHARS: usize = 8;

/// 主密码输入框的稳定 id：`egui` 靠它记住光标，测试靠它把焦点放对地方
pub const PASSWORD_FIELD_ID: &str = "password_field";
/// 第二次输入（设密码时）的输入框 id
pub const CONFIRM_FIELD_ID: &str = "confirm_field";

/// 解锁 / 设密码卡片
#[derive(Default)]
pub struct UnlockView {
    /// 输入的主密码
    password: Zeroizing<String>,
    /// 第二次输入（只有设置密码时用）
    confirm: Zeroizing<String>,
    /// 界面自己就能看出来的问题（太短、两次不一样）
    local_error: Option<String>,
    /// 下一帧把焦点给第一个输入框
    focus_pending: bool,
}

impl UnlockView {
    pub fn new() -> Self {
        Self {
            focus_pending: true,
            ..Default::default()
        }
    }

    /// 画一帧。`external_error` 是存储层带回来的失败（密码不对、库坏了……）。
    /// 返回是否提交。
    pub fn show(&mut self, ui: &mut Ui, mode: UnlockMode, external_error: Option<&str>) -> bool {
        let mut submitted = false;

        theme::column(ui, 380.0, |ui| {
            // 视线落点略高于正中，卡片不至于把页面压得很沉
            ui.add_space((ui.available_height() * 0.16).max(16.0));

            egui::Frame::window(ui.style())
                .inner_margin(Margin::symmetric(28, 26))
                .show(ui, |ui| {
                    ui.set_width(324.0);
                    shell::wordmark(ui);

                    ui.add_space(22.0);
                    let (title, subtitle, cta) = match mode {
                        UnlockMode::Setup => (
                            "先设一个主密码",
                            "密码不会保存，忘了就打不开这本日记",
                            "开始写",
                        ),
                        UnlockMode::Unlock => ("输入主密码", "解锁之后才能读写日记", "解锁"),
                    };
                    ui.label(
                        RichText::new(title)
                            .font(theme::display(19.0))
                            .color(theme::INK_DEEP),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(subtitle)
                            .font(theme::sans(12.0))
                            .color(theme::MUTED),
                    );

                    ui.add_space(18.0);
                    let password = password_field(
                        ui,
                        PASSWORD_FIELD_ID,
                        "主密码",
                        &mut self.password,
                        self.focus_pending,
                    );
                    let mut confirm = None;
                    if mode == UnlockMode::Setup {
                        ui.add_space(8.0);
                        // 焦点只给第一个框：一帧里两次 request_focus，赢的是后一个
                        confirm = Some(password_field(
                            ui,
                            CONFIRM_FIELD_ID,
                            "再输一遍",
                            &mut self.confirm,
                            false,
                        ));
                    }
                    // eframe 把窗口藏着建、第一帧画完才显形：还没显形时窗口收不到键盘
                    // （`has_focus` 为假），这一帧就别把"下次聚焦"用掉，等真能敲键盘再落实
                    if ui.ctx().input(|i| i.focused) {
                        self.focus_pending = false;
                    }

                    // 回车提交：单行输入框按回车会失焦，顺势当作"提交"
                    let entered = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let blurred = password.lost_focus()
                        || confirm.is_some_and(|response| response.lost_focus());
                    if entered && blurred {
                        submitted = true;
                    }

                    if let Some(message) = self.local_error.as_deref().or(external_error) {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(format!("⚠ {message}"))
                                .font(theme::sans(11.5))
                                .color(theme::RED_FG),
                        );
                    }

                    ui.add_space(18.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(match mode {
                                UnlockMode::Setup => format!("至少 {MIN_PASSWORD_CHARS} 个字符"),
                                UnlockMode::Unlock => "".to_owned(),
                            })
                            .font(theme::sans(11.0))
                            .color(theme::MUTED),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::primary_button(ui, cta).clicked() {
                                submitted = true;
                            }
                        });
                    });
                });
        });

        if submitted {
            // 界面能自己看出来就先别去动磁盘
            self.local_error = validate(mode, &self.password, &self.confirm);
            submitted = self.local_error.is_none();
        }
        submitted
    }

    /// 待提交的主密码。只有 [`UnlockView::show`] 返回 true 时才有意义。
    pub fn password(&self) -> &str {
        self.password.as_str()
    }

    /// 清空输入并擦掉缓冲区，下一帧重新聚焦
    pub fn clear(&mut self) {
        self.password.zeroize();
        self.confirm.zeroize();
        self.local_error = None;
        self.focus_pending = true;
    }
}

/// 界面自己就能判断的输入问题
fn validate(mode: UnlockMode, password: &str, confirm: &str) -> Option<String> {
    if mode != UnlockMode::Setup {
        // 解锁时密码对不对只有存储层知道，不在这里猜
        return None;
    }
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Some(format!("密码至少 {MIN_PASSWORD_CHARS} 个字符"));
    }
    if password != confirm {
        return Some("两次输入的密码不一样".to_owned());
    }
    None
}

/// 带边框的密码输入框。
///
/// 框自己画：全局样式把输入框的底和边都做成了透明，靠它画不出"这里能输入"的暗示。
fn password_field(
    ui: &mut Ui,
    id: &str,
    hint: &str,
    buffer: &mut String,
    focus: bool,
) -> egui::Response {
    const HEIGHT: f32 = 38.0;
    const PADDING_X: f32 = 11.0;

    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), HEIGHT), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(8), theme::SIDEBAR);
    painter.rect_stroke(
        rect.shrink(0.5),
        CornerRadius::same(8),
        Stroke::new(1.0, theme::HAIRLINE),
        StrokeKind::Inside,
    );

    let inner = egui::Rect::from_min_max(
        egui::pos2(rect.left() + PADDING_X, rect.top()),
        egui::pos2(rect.right() - PADDING_X, rect.bottom()),
    );
    let response = ui
        .scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(buffer)
                        .id(egui::Id::new(id))
                        .frame(egui::Frame::NONE)
                        .password(true)
                        .hint_text(hint)
                        .font(theme::sans(14.0))
                        .text_color(theme::INK)
                        .margin(Margin::symmetric(0, 9))
                        .desired_width(f32::INFINITY),
                )
            })
            .inner
        })
        .inner;

    if focus {
        response.request_focus();
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_requires_a_long_enough_matching_password() {
        assert!(
            validate(UnlockMode::Setup, "1234567", "1234567").is_some(),
            "七个字符太短"
        );
        assert!(
            validate(UnlockMode::Setup, "12345678", "12345679").is_some(),
            "两次不一致"
        );
        assert!(validate(UnlockMode::Setup, "12345678", "12345678").is_none());
        // 按字符数而不是字节数算：八个汉字也是八个字符
        assert!(validate(UnlockMode::Setup, "一二三四五六七八", "一二三四五六七八").is_none());
    }

    #[test]
    fn unlocking_does_not_judge_the_password_locally() {
        assert!(validate(UnlockMode::Unlock, "", "").is_none());
    }

    #[test]
    fn clearing_wipes_the_buffer() {
        let mut view = UnlockView::new();
        view.password.push_str("不该留在内存里");
        view.clear();
        assert_eq!(view.password(), "");
    }

    #[test]
    fn a_frame_without_input_does_not_submit() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut view = UnlockView::new();

        let mut submitted = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                submitted = Some(view.show(ui, UnlockMode::Setup, None));
            });
        });
        assert_eq!(submitted, Some(false));
        // 测试里没有渲染后端，纹理增量得自己收掉
        output.textures_delta.clear();
    }
}
