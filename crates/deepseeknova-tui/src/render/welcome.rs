//! 全屏欢迎屏：顶部信息栏 + 居中 logo + 菜单 + 配置警示。
//!
//! 对齐 grok build 的 welcome 屏结构（top_bar → logo → menu → prompt）：
//! - 顶部信息栏：工作区 `{cwd}:{branch}`（左）+ 模型标签（右）；
//! - 垂直居中：ASCII logo → 副标题 → 菜单（新对话/恢复会话/命令面板/帮助）；
//! - 配置警示行（provider / API key 缺失时的冷启动引导）；
//! - 提示词输入不在此渲染——由底部输入区（`render::input::render_input`）
//!   统一承担（用户直接键入，Enter 提交后 `welcome` 清除回到对话布局）。

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::state::AppState;
use crate::i18n::Key;
use crate::theme::Theme;

/// 欢迎屏 logo 行（我们自己的 DEEPSEEKN 字标，6 行大号 ASCII 方块字）。
///
/// 每字母约 11 列、行高 6，保证在终端上清晰可读（实测反馈此前 5 行
/// 紧凑字标"完全看不出来"）。标志是 DeepseekNova 自己的（DEEPSEEKN），
/// 渲染套用 grok build 的 shimmer 扫光动画（见 [`shimmer_spans`]）——
/// 只借动效、不借标志。居中显示在屏幕正中。
fn logo_lines() -> Vec<&'static str> {
    vec![
        " ██████╗ ███████╗███████╗██████╗ ███████╗███████╗███████╗██╗  ██╗ ██████╗ ",
        " ██╔══██╗██╔════╝██╔════╝██╔══██╗██╔════╝██╔════╝██╔════╝██║  ██║██╔═══██╗",
        " ██║  ██║█████╗  █████╗  ██████╔╝█████╗  █████╗  █████╗  ██║  ██║██║   ██║",
        " ██║  ██║██╔══╝  ██╔══╝  ██╔═══╝ ╚════██║██╔══╝  ██╔══╝  ██║  ██║██║   ██║",
        " ██████╔╝███████╗███████╗██║     ███████║███████╗███████╗███████╗╚██████╔╝",
        " ╚═════╝ ╚══════╝╚══════╝╚═╝     ╚══════╝╚══════╝╚══════╝╚══════╝ ╚═════╝ ",
    ]
}

/// shimmer 扫光动画相位（秒，自进程启动起算；与帧率解耦，墙钟驱动）。
fn shimmer_phase_secs() -> f32 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f32()
}

/// 单个 braille 字符的扫光亮度（0.0 静默灰 → 1.0 全亮 text_primary）。
///
/// 一条 raised-cosine 亮带沿「左下 → 右上」对角线扫过 logo（grok
/// `shine_opacity` 同款），4 秒一个周期（约 1.3s 扫光 + 其余静默），
/// 叠加 5 秒周期的轻微呼吸脉冲。
///
/// 强度相比 grok 上调（SHINE 0.33 → 0.55、PULSE 0.06 → 0.10）：深色
/// 终端上 gray → text_primary 的静默色差极弱，强度不足时扫光几乎不可见
/// （实测反馈"渲染风格完全没看到"）。
fn shine_opacity(diag: f32, secs: f32) -> f32 {
    const BAND: f32 = 0.38;
    const CYCLE: f32 = 4.0;
    const SWEEP_FRAC: f32 = 0.32;
    const SHINE: f32 = 0.55;
    const PULSE: f32 = 0.10;
    const PULSE_SECS: f32 = 5.0;

    let p = (secs % CYCLE) / CYCLE;
    let q = (p / SWEEP_FRAC).min(1.0);
    let band_pos = -BAND + q * (1.0 + 2.0 * BAND);
    let pulse = PULSE * (0.5 - 0.5 * (std::f32::consts::TAU * secs / PULSE_SECS).cos());

    let d = (diag - band_pos).abs();
    let shine = if d < BAND {
        0.5 * (1.0 + (std::f32::consts::PI * d / BAND).cos())
    } else {
        0.0
    };
    (pulse + SHINE * shine).clamp(0.0, 1.0)
}

/// 两个颜色间的线性插值（t 在 0.0..=1.0）；任一端非 RGB 时就近取端点。
fn blend_color(
    a: ratatui::style::Color,
    b: ratatui::style::Color,
    t: f32,
) -> ratatui::style::Color {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return if t < 0.5 { a } else { b };
    };
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

/// 把 braille logo 渲染为 shimmer 扫光行组（每行 span 按扫光亮度着色）。
fn shimmer_spans(theme: &Theme) -> Vec<Line<'static>> {
    use ratatui::style::Color;
    let rows = logo_lines().len() as f32;
    let cols = logo_lines()
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1) as f32;
    let secs = shimmer_phase_secs();
    let base = theme.gray;
    let hilite = theme.text_primary;
    logo_lines()
        .iter()
        .enumerate()
        .map(|(row, line)| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut run = String::new();
            let mut run_color: Option<Color> = None;
            for (col, ch) in line.chars().enumerate() {
                // 沿「左下 → 右上」对角线：col 增大、row 减小 → diag 增大。
                let diag = (col as f32 + (rows - 1.0 - row as f32)) / (cols + rows);
                let color = blend_color(base, hilite, shine_opacity(diag, secs));
                if run_color != Some(color) {
                    if let Some(prev) = run_color {
                        spans.push(Span::styled(
                            std::mem::take(&mut run),
                            Style::default().fg(prev),
                        ));
                    }
                    run_color = Some(color);
                }
                run.push(ch);
            }
            if let Some(prev) = run_color {
                spans.push(Span::styled(run, Style::default().fg(prev)));
            }
            Line::from(spans)
        })
        .collect()
}

/// 菜单项：标签键 + 触发说明（快捷键/命令）。
fn menu_entries() -> Vec<(Key, &'static str)> {
    vec![
        (Key::WelcomeMenuNew, "Enter"),
        (Key::WelcomeMenuResume, "/sessions"),
        (Key::WelcomeMenuPalette, "/"),
        (Key::WelcomeMenuHelp, "F1"),
    ]
}

/// 顶部信息栏：`{cwd}:{branch}`（左）+ 模型标签（右）。
fn top_bar_spans(app: &AppState, theme: &Theme) -> Line<'static> {
    let branch = app.git_branch.clone().unwrap_or_default();
    let location = if branch.is_empty() {
        app.workspace_cwd.clone()
    } else {
        format!("{}:{}", app.workspace_cwd, branch)
    };
    let model = if app.model_label.is_empty() {
        String::new()
    } else {
        format!("⚡ {}", app.model_label)
    };
    Line::from(vec![
        Span::styled(location, theme.system),
        Span::styled(" ".repeat(4), Style::default()),
        Span::styled(
            model,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

/// 配置警示行：provider / API key 缺失时展示（冷启动引导）。
fn warning_lines(app: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    let warn = |s: String| {
        Line::from(Span::styled(
            s,
            Style::default()
                .fg(theme.danger)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let mut lines = Vec::new();
    if !app.provider_configured {
        lines.push(warn(app.tr.t(Key::WelcomeNoProvider).to_string()));
    } else if !app.api_key_configured {
        lines.push(warn(app.tr.t(Key::WelcomeNoApiKey).to_string()));
    }
    lines
}

/// 渲染全屏欢迎屏（调用方保证 `app.welcome` 激活且无对话）。
///
/// 布局（用户要求）：**我们的标志水平垂直居中在屏幕正中**，副标题、
/// 菜单、配置警示依次放在 logo 下方（统一居中，不再分宽窄两列 hero_box）。
pub fn render_welcome(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    // 顶部信息栏（1 行）。
    let bar_area = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: 1,
    };
    f.render_widget(Paragraph::new(top_bar_spans(app, theme)), bar_area);

    // logo（shimmer 扫光）→ 副标题 → 菜单 → 警示：整体垂直居中。
    let shimmer = shimmer_spans(theme);
    let subtitle = app.tr.t(Key::WelcomeSubtitle);
    let warnings = warning_lines(app, theme);
    let menu = menu_entries();
    let content_h = shimmer.len() as u16 + 1 + 1 + menu.len() as u16 + warnings.len() as u16 + 2;
    let top = area.y + area.height.saturating_sub(content_h) / 2;
    let content_area = Rect {
        x: area.x,
        y: top,
        width: area.width,
        height: content_h,
    };

    // logo：水平居中（每行 Center），垂直位于内容区顶部（即屏幕正中区块）。
    let mut y = content_area.y;
    for line in &shimmer {
        if y >= area.bottom() {
            break;
        }
        f.render_widget(
            Paragraph::new(line.clone()).alignment(Alignment::Center),
            Rect {
                x: content_area.x,
                y,
                width: content_area.width,
                height: 1,
            },
        );
        y += 1;
    }

    // 副标题（logo 下方，居中）。
    y += 1;
    if y < area.bottom() {
        f.render_widget(
            Paragraph::new(Span::styled(subtitle, theme.system)).alignment(Alignment::Center),
            Rect {
                x: content_area.x,
                y,
                width: content_area.width,
                height: 1,
            },
        );
    }

    // 菜单（副标题下方，居中；hover 高亮与首项选中态）。
    y += 2;
    for (i, (label_key, shortcut)) in menu.iter().enumerate() {
        if y >= area.bottom() {
            break;
        }
        let selected = i == 0;
        let hovered = app.mouse_pos.is_some_and(|(_, row)| row == y);
        let line = Line::from(vec![
            Span::styled("› ", Style::default().fg(theme.accent_user)),
            Span::styled(
                app.tr.t(*label_key),
                Style::default()
                    .fg(if selected || hovered {
                        theme.text_primary
                    } else {
                        theme.gray
                    })
                    .add_modifier(if selected || hovered {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(format!("  {shortcut}"), Style::default().fg(theme.gray_dim)),
        ]);
        f.render_widget(
            Paragraph::new(line).alignment(Alignment::Center),
            Rect {
                x: content_area.x,
                y,
                width: content_area.width,
                height: 1,
            },
        );
        y += 1;
    }

    // 配置警示（菜单下方，居中）。
    y += 1;
    for line in warnings {
        if y >= area.bottom() {
            break;
        }
        f.render_widget(
            Paragraph::new(line).alignment(Alignment::Center),
            Rect {
                x: content_area.x,
                y,
                width: content_area.width,
                height: 1,
            },
        );
        y += 1;
    }

    // grok 对齐：welcome 屏接入 toast（底部输入区上方）。
    crate::render::render_toast(app, theme, f, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_app() -> AppState {
        AppState {
            model_label: "deepseek-v4".into(),
            workspace_cwd: "/workspace".into(),
            git_branch: Some("main".into()),
            ..Default::default()
        }
    }

    #[test]
    fn logo_lines_have_six_rows() {
        // 我们自己的 DEEPSEEKN 大字标为 6 行（清晰可读，每字母 ~11 列）。
        assert_eq!(logo_lines().len(), 6);
    }

    #[test]
    fn menu_entries_map_four_actions() {
        let entries = menu_entries();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].0, Key::WelcomeMenuNew);
        assert_eq!(entries[1].0, Key::WelcomeMenuResume);
    }

    #[test]
    fn renders_without_panic() {
        let app = sample_app();
        let theme = Theme::default();
        let buf = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_welcome(&app, &theme, f, area);
            })
            .unwrap();
    }

    #[test]
    fn renders_in_narrow_terminal_without_overflow() {
        let app = sample_app();
        let theme = Theme::default();
        let buf = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        // 窄终端：内容溢出时只渲染放得下的部分，不得 panic。
        terminal
            .draw(|f| {
                let area = f.area();
                render_welcome(&app, &theme, f, area);
            })
            .unwrap();
    }
}
