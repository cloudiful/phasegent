//! The comment-preserving version-1 to version-2 document transform.
//!
//! The transform never regenerates the document from the model. It reads the
//! original text through the CST, keeps every comment the operator wrote,
//! moves each comment with the value it belonged to, and carries the role
//! subtree over unchanged.

use crate::infra::config_file::SettingValue;
use crate::infra::config_file::setting_path::GlobalSetting;
use jsonc_parser::cst::{CstNode, CstRootNode};

/// One migrated setting: its destination, its typed value, and the comments
/// the operator wrote next to it in the version-1 file.
pub struct MigratedSetting {
    /// The destination of the setting inside the version-2 tree.
    pub setting: &'static GlobalSetting,
    /// The typed value the version-2 tree stores.
    pub value: SettingValue,
    /// The comment lines that preceded the setting in the version-1 file.
    pub comments: Vec<String>,
}

/// One entry of the rendered version-2 tree.
enum Node {
    /// A typed value.
    Leaf { json: String, comments: Vec<String> },
    /// A nested section.
    Branch(Vec<(String, Node)>),
}

/// Render the version-2 document of a version-1 file.
///
/// The roles subtree is copied verbatim, so an unrelated role entry keeps its
/// exact text, its comments, and its formatting.
pub fn render(root: &CstRootNode, migrated: &[MigratedSetting]) -> Result<String, String> {
    let Some(value) = root.value() else {
        return Err("the configuration file must contain a top-level object".to_owned());
    };
    let Some(object) = value.as_object() else {
        return Err("the configuration file must contain a top-level object".to_owned());
    };

    let mut header: Vec<String> = Vec::new();
    let mut settings_comments: Vec<String> = Vec::new();
    let mut roles_comments: Vec<String> = Vec::new();
    let mut roles_text: Option<String> = None;
    let mut pending: Vec<String> = Vec::new();
    let mut first_property = true;
    for child in object.children() {
        if child.is_comment() {
            pending.push(comment_text(&child));
            continue;
        }
        let Some(property) = child.as_object_prop() else {
            continue;
        };
        if first_property {
            header = std::mem::take(&mut pending);
            first_property = false;
        }
        let name = property.decoded_name().unwrap_or_default();
        match name.as_str() {
            "settings" => settings_comments = std::mem::take(&mut pending),
            "roles" => {
                roles_comments = std::mem::take(&mut pending);
                roles_text = property.value().map(|node| node.to_string());
            }
            _ => pending.clear(),
        }
    }
    let trailing = pending;

    let mut branches: Vec<(String, Node)> = Vec::new();
    for entry in migrated {
        let leaf = Node::Leaf {
            json: render_value(&entry.value)?,
            comments: entry.comments.clone(),
        };
        insert(&mut branches, entry.setting.segments, leaf)?;
    }

    let mut out = String::new();
    let mut after_root = false;
    let mut footer = Vec::new();
    for child in root.children() {
        if !child.is_trivia() {
            after_root = true;
        } else if child.is_comment() {
            if after_root {
                footer.push(child.to_string());
            } else {
                out.push_str(&child.to_string());
                out.push('\n');
            }
        }
    }
    out.push_str("{\n");
    emit_comments(&header, 1, &mut out);
    out.push_str("  \"version\": 2");
    if !branches.is_empty() {
        out.push_str(",\n");
        emit_comments(&settings_comments, 1, &mut out);
        for (index, (name, node)) in branches.iter().enumerate() {
            if index > 0 {
                out.push_str(",\n");
            }
            emit_node(name, node, 1, &mut out);
        }
    }
    if roles_text.is_some() {
        out.push_str(",\n");
        emit_comments(&roles_comments, 1, &mut out);
        out.push_str("  \"roles\": ");
        out.push_str(roles_text.as_deref().unwrap_or_default());
    } else {
        out.push_str(",\n  \"roles\": {}");
    }
    if !trailing.is_empty() {
        out.push('\n');
        emit_comments(&trailing, 1, &mut out);
    }
    out.push_str("\n}\n");
    emit_comments(&footer, 0, &mut out);

    let rendered_root = crate::infra::config_file::parse_root(&out)?;
    let mut existing = std::collections::BTreeMap::<String, usize>::new();
    for child in rendered_root.children() {
        collect_comments(&child, &mut |comment| {
            *existing.entry(comment).or_default() += 1
        });
    }
    let mut missing = Vec::new();
    for child in root.children() {
        collect_comments(&child, &mut |comment| match existing.get_mut(&comment) {
            Some(count) if *count > 0 => *count -= 1,
            _ => missing.push(comment),
        });
    }
    emit_comments(&missing, 0, &mut out);
    Ok(out)
}

/// Insert a value into the tree at its destination path.
fn insert(branches: &mut Vec<(String, Node)>, segments: &[&str], leaf: Node) -> Result<(), String> {
    let Some((first, rest)) = segments.split_first() else {
        return Err("the configuration path is empty".to_owned());
    };
    if rest.is_empty() {
        branches.push(((*first).to_owned(), leaf));
        return Ok(());
    }
    if let Some(entry) = branches.iter_mut().find(|(name, _)| name == first) {
        return match &mut entry.1 {
            Node::Branch(children) => insert(children, rest, leaf),
            Node::Leaf { .. } => Err(
                "the configuration tree holds a value and a section at the same path".to_owned(),
            ),
        };
    }
    let mut children: Vec<(String, Node)> = Vec::new();
    insert(&mut children, rest, leaf)?;
    branches.push(((*first).to_owned(), Node::Branch(children)));
    Ok(())
}

/// Emit one entry of the tree at the given depth.
fn emit_node(name: &str, node: &Node, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    match node {
        Node::Leaf { json, comments } => {
            emit_comments(comments, depth, out);
            out.push_str(&format!("{indent}\"{name}\": {json}"));
        }
        Node::Branch(children) => {
            out.push_str(&format!("{indent}\"{name}\": {{\n"));
            for (index, (child_name, child)) in children.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                emit_node(child_name, child, depth + 1, out);
            }
            out.push_str(&format!("\n{indent}}}"));
        }
    }
}

/// Emit the comment lines at the given depth.
fn emit_comments(comments: &[String], depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    for comment in comments {
        out.push_str(&indent);
        out.push_str(comment);
        out.push('\n');
    }
}

fn collect_comments(node: &CstNode, visit: &mut impl FnMut(String)) {
    if node.is_comment() {
        visit(node.to_string());
    } else {
        for child in node.children() {
            collect_comments(&child, visit);
        }
    }
}

/// The text of a comment node.
fn comment_text(node: &CstNode) -> String {
    node.to_string()
}

/// Render a typed value as JSON.
fn render_value(value: &SettingValue) -> Result<String, String> {
    serde_json::to_string(&value.to_json())
        .map_err(|_| "could not render the migrated value".to_owned())
}
