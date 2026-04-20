use console::style;

pub mod log {
    use super::*;

    pub fn info(msg: &str) {
        println!("{}", style(msg).dim());
    }

    pub fn info_important(msg: &str) {
        println!("{}", msg);
    }

    pub fn warn(msg: &str) {
        println!("{}", style(msg).yellow());
    }

    pub fn error(msg: &str) {
        eprintln!("{}", style(msg).red());
    }

    pub fn success(msg: &str) {
        println!("{}", style(msg).green());
    }

    /// Print a heading with green background + powerline segment (for milestones)
    pub fn heading_note(title: &str) {
        println!(
            "{}{}{}{}",
            style("  ").on_green(),
            style(title).white().bold().on_green(),
            style("  ").on_green(),
            style("\u{E0B0}").green()
        );
    }

    /// Print a heading with bright blue background + ⬝ prefix + powerline segment (for steps)
    pub fn heading_info(title: &str) {
        println!(
            "{}{}{}{}",
            style("  ⬝").white().bold().on_blue().on_bright(),
            style(title).white().bold().on_blue().on_bright(),
            style("   ").on_blue().on_bright(),
            style("\u{E0B0}").blue().bright()
        );
    }
}
