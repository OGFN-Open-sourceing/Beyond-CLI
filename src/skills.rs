//! Skills: markdown files in `ExtraResources/Skills/` that teach the agent
//! recurring workflows, project conventions and procedures. Loaded at boot and
//! injected into the system prompt; the `write_skill` tool lets the agent grow
//! this library as it learns from projects.

use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: String,
    /// Full markdown body (without frontmatter).
    pub content: String,
}

impl Skill {
    /// Parse a `*.md` skill file, tolerating missing frontmatter.
    fn from_file(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let mut name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "skill".into());
        let mut description = String::new();
        let mut content = raw.as_str();

        // Optional `---` frontmatter with `name:` / `description:`.
        if raw.starts_with("---")
            && let Some(end) = raw[3..].find("\n---") {
                let fm = &raw[3..3 + end];
                for line in fm.lines() {
                    if let Some(v) = line.trim().strip_prefix("name:") {
                        name = v.trim().to_string();
                    } else if let Some(v) = line.trim().strip_prefix("description:") {
                        description = v.trim().to_string();
                    }
                }
                content = &raw[3 + end + 4..]; // skip closing `---`
            }

        Ok(Self { name, description, path: path.display().to_string(), content: content.trim().to_string() })
    }
}

/// Load every `*.md` skill under `dir` (sorted by name).
pub fn load_all(dir: &Path) -> Vec<Skill> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() && p.extension().map(|x| x == "md").unwrap_or(false) {
            match Skill::from_file(&p) {
                Ok(s) => out.push(s),
                Err(_) => continue, // unreadable skill: skip silently
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Build the skills section of the system prompt (truncated to stay lean).
pub fn system_prompt_section(skills: &[Skill], per_skill_chars: usize) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n## Available skills\nYou may consult these learned skills; follow them when relevant to the task:\n",
    );
    for sk in skills {
        let desc = if sk.description.is_empty() { "learned workflow" } else { &sk.description };
        s.push_str(&format!(
            "\n### Skill: {} — {}\n({})\n{}\n",
            sk.name,
            desc,
            sk.path,
            crate::util::truncate_chars(&sk.content, per_skill_chars)
        ));
    }
    s
}
