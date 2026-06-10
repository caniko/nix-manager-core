use console::{Term, style};
use dialoguer::Input;
use dialoguer::theme::ColorfulTheme;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub fn header(msg: impl AsRef<str>) {
    eprintln!(
        "{} {}",
        style("▶").cyan().bold(),
        style(msg.as_ref()).bold()
    );
}

pub fn step(msg: impl AsRef<str>) {
    eprintln!("  {} {}", style("·").dim(), msg.as_ref());
}

pub fn success(msg: impl AsRef<str>) {
    eprintln!("{} {}", style("✔").green().bold(), msg.as_ref());
}

pub fn step_success(msg: impl AsRef<str>) {
    eprintln!("  {} {}", style("✔").green().bold(), msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    eprintln!(
        "{} {}",
        style("⚠").yellow().bold(),
        style(msg.as_ref()).yellow()
    );
}

pub fn error(msg: impl AsRef<str>) {
    eprintln!("{} {}", style("✖").red().bold(), style(msg.as_ref()).red());
}

pub fn info(msg: impl AsRef<str>) {
    eprintln!("  {}", style(msg.as_ref()).dim());
}

pub fn key_value(key: &str, value: impl AsRef<str>) {
    eprintln!(
        "  {:<10} {}",
        style(format!("{key}:")).dim(),
        value.as_ref()
    );
}

pub fn spinner(msg: impl Into<String>) -> ProgressBar {
    let pb = if Term::stderr().is_term() {
        ProgressBar::new_spinner()
    } else {
        ProgressBar::hidden()
    };
    pb.set_style(
        ProgressStyle::with_template("  {spinner:.cyan} {msg} {elapsed:.dim}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    pb.set_message(msg.into());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

pub fn finish_spinner(pb: ProgressBar, msg: impl Into<String>) {
    let elapsed = pb.elapsed();
    let formatted = format!(
        "{} {}",
        msg.into(),
        style(format!("({})", format_elapsed(elapsed))).dim(),
    );
    pb.finish_and_clear();
    eprintln!("  {} {}", style("✔").green().bold(), formatted);
}

pub fn fail_spinner(pb: ProgressBar, msg: impl Into<String>) {
    pb.finish_and_clear();
    eprintln!("  {} {}", style("✖").red().bold(), style(msg.into()).red());
}

fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m {}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    }
}

pub fn confirm_typed(prompt: &str, expected: &str) -> anyhow::Result<bool> {
    let theme = ColorfulTheme::default();
    let answer: String = Input::with_theme(&theme)
        .with_prompt(prompt)
        .allow_empty(true)
        .interact_text()?;
    Ok(answer.trim() == expected)
}
