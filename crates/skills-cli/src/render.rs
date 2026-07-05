//! Plain-text report rendering (ASCII only: Windows consoles are a
//! first-class target).

use skills_core::domain::NoteKind;
use skills_core::lockfile::SyncStatus;
use skills_core::pipeline::{SyncAction, SyncReport};

pub fn render_update(report: &SyncReport) -> String {
    let mut out = String::new();
    if report.dry_run {
        out.push_str(&format!(
            "Dry run: would sync skills into {} (no files will be written)\n",
            report.target_rel
        ));
    } else {
        out.push_str(&format!("Syncing skills into {}\n", report.target_rel));
    }

    let mut current_vendor: Option<&str> = None;
    for entry in &report.entries {
        if current_vendor != Some(entry.vendor.as_str()) {
            out.push_str(&format!("\n{}:\n", entry.vendor));
            current_vendor = Some(entry.vendor.as_str());
        }
        let files = plural(entry.file_count);
        let line = match (entry.action, report.dry_run) {
            (SyncAction::Add, false) => format!("[add]  {} (copied {files})", entry.id),
            (SyncAction::Add, true) => format!("[add]  {} (would copy {files})", entry.id),
            (SyncAction::Update, false) => format!("[upd]  {} (copied {files})", entry.id),
            (SyncAction::Update, true) => format!("[upd]  {} (would copy {files})", entry.id),
            (SyncAction::Remove, false) => format!("[del]  {} (removed {files})", entry.id),
            (SyncAction::Remove, true) => format!("[del]  {} (would remove {files})", entry.id),
            (SyncAction::Skip, _) => format!("[ok]   {} (up to date)", entry.id),
        };
        out.push_str(&format!("  {line}\n"));
    }

    if report.entries.is_empty() {
        out.push_str("\nNo skills found.\n");
    } else {
        out.push_str(&format!(
            "\n{} added, {} updated, {} removed, {} up to date.\n",
            report.count(SyncAction::Add),
            report.count(SyncAction::Update),
            report.count(SyncAction::Remove),
            report.count(SyncAction::Skip),
        ));
    }

    if !report.notes.is_empty() {
        out.push('\n');
        for note in &report.notes {
            let tag = match note.kind {
                NoteKind::Skip => "[skip]",
                NoteKind::Hint => "[hint]",
            };
            out.push_str(&format!("{tag} {}\n", note.message));
        }
    }
    out
}

fn plural(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

pub struct ShowVendor {
    pub name: String,
    pub lines: Vec<ShowLine>,
}

pub struct ShowLine {
    pub id: String,
    pub description: Option<String>,
    pub status: SyncStatus,
}

pub fn render_show(target_rel: &str, vendors: &[ShowVendor]) -> String {
    let mut out = String::new();
    out.push_str(&format!("Target: {target_rel}\n"));

    if vendors.is_empty() {
        out.push_str("\nNo donors configured (add local.dir entries to skills.json).\n");
        return out;
    }

    let id_width = vendors
        .iter()
        .flat_map(|v| v.lines.iter())
        .map(|l| l.id.len())
        .max()
        .unwrap_or(0);
    let desc_width = vendors
        .iter()
        .flat_map(|v| v.lines.iter())
        .map(|l| l.description.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(1);

    for vendor in vendors {
        out.push_str(&format!("\n{}:\n", vendor.name));
        if vendor.lines.is_empty() {
            out.push_str("  (no skills)\n");
            continue;
        }
        for line in &vendor.lines {
            let status = match line.status {
                SyncStatus::Synced => "[ok]",
                SyncStatus::Modified => "[mod]",
                SyncStatus::NotSynced => "not-synced",
            };
            let desc = line.description.as_deref().unwrap_or("-");
            out.push_str(&format!(
                "  {:<id_width$}  {:<desc_width$}  {status}\n",
                line.id, desc
            ));
        }
    }
    out
}
