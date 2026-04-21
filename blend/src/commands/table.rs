use crate::compose::{discover_packages, get_order_package};
use crate::context::Context;
use crate::output::log;

/// Table command: output package info as HTML table for README
pub fn cmd_table(ctx: &Context) -> anyhow::Result<()> {
    let packages = discover_packages(&ctx.orders_dir);

    let profiles: &[(&str, &str, &str)] = &[
        ("linux", "x86_64", "linux-x86_64"),
        ("darwin", "x86_64", "macos-x86_64"),
        ("darwin", "aarch64", "macos-aarch64"),
    ];

    let mut pkg_data: Vec<(String, Vec<bool>, usize)> = Vec::new();

    for pkg in &packages {
        match get_order_package(ctx, pkg) {
            Ok(order_pkg) => {
                let matches: Vec<bool> = profiles
                    .iter()
                    .map(|(os, arch, _)| order_pkg.applies_on_platform(os, arch))
                    .collect();
                let match_count = matches.iter().filter(|&&m| m).count();
                pkg_data.push((pkg.clone(), matches, match_count));
            }
            Err(e) => {
                log::warn(&format!("Skipping {pkg} (eval error: {e})"));
            }
        }
    }

    pkg_data.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

    print!("<table><thead><tr><th>package</th><th colspan=\"3\">profiles</th></tr></thead><tbody>");
    for (name, matches, _) in &pkg_data {
        print!("\n<tr><td><a href=\"orders/{name}\">{name}</a></td>");
        for (i, (_os, _arch, label)) in profiles.iter().enumerate() {
            if matches[i] {
                print!("<td><code>{label}</code></td>");
            } else {
                print!("<td><code>&nbsp;...</code></td>");
            }
        }
        print!("</tr>");
    }
    println!("\n</tbody></table>");

    Ok(())
}
