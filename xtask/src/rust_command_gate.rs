//! Detect literal interpreter launches in Rust syntax, not comments or text.

use std::collections::BTreeSet;

use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, Item, ItemMod, ItemUse, Lit, Stmt, UseTree};

#[cfg(test)]
#[path = "rust_command_gate_tests.rs"]
mod tests;

#[derive(Clone, Default)]
struct Names {
    commands: BTreeSet<String>,
    modules: BTreeSet<String>,
}

/// Returns the number of literal interpreter launches in a Rust source file.
/// Expressions and shells which construct a command at runtime require separate review.
pub(super) fn scan(source: &str) -> Result<usize, syn::Error> {
    let file = syn::parse_file(source)?;
    let mut visitor = Scanner {
        names: imports(file.items.iter(), &Names::default()),
        parent: Names::default(),
        matches: 0,
    };
    visitor.visit_file(&file);
    Ok(visitor.matches)
}

#[derive(Default)]
struct Scanner {
    names: Names,
    parent: Names,
    matches: usize,
}

impl<'ast> Visit<'ast> for Scanner {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        let previous = std::mem::take(&mut self.names);
        let previous_parent = std::mem::replace(&mut self.parent, previous.clone());
        if let Some((_, items)) = &node.content {
            self.names = imports(items.iter(), &self.parent);
            for item in items {
                self.visit_item(item);
            }
        }
        self.names = previous;
        self.parent = previous_parent;
    }

    fn visit_block(&mut self, node: &'ast syn::Block) {
        let previous = self.names.clone();
        for statement in &node.stmts {
            if let Stmt::Item(Item::Use(item)) = statement {
                collect_use(item, &mut self.names, &self.parent);
            }
        }
        for statement in &node.stmts {
            if let Stmt::Item(item) = statement
                && let Some(name) = declared_type(item)
            {
                self.names.commands.remove(&name);
                self.names.modules.remove(&name);
            }
        }
        visit::visit_block(self, node);
        self.names = previous;
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(path) = node.func.as_ref()
            && path.qself.is_none()
            && is_command_constructor(&path.path, &self.names, &self.parent)
            && let Some(Expr::Lit(value)) = node.args.first()
            && let Lit::Str(value) = &value.lit
        {
            let value = value.value();
            let filename = value.rsplit(['/', '\\']).next().unwrap_or(&value);
            if super::interpreter_name(&filename.to_ascii_lowercase()) {
                self.matches += 1;
            }
        }
        visit::visit_expr_call(self, node);
    }
}

fn declared_type(item: &Item) -> Option<String> {
    let identifier = match item {
        Item::Struct(item) => &item.ident,
        Item::Enum(item) => &item.ident,
        Item::Union(item) => &item.ident,
        Item::Type(item) => &item.ident,
        Item::Trait(item) => &item.ident,
        Item::Mod(item) => &item.ident,
        _ => return None,
    };
    Some(identifier.to_string())
}

fn imports<'a>(items: impl Iterator<Item = &'a Item> + Clone, parent: &Names) -> Names {
    let mut names = Names::default();
    for item in items.clone() {
        if let Item::Use(item) = item {
            collect_use(item, &mut names, parent);
        }
    }
    for item in items {
        if let Some(name) = declared_type(item) {
            names.commands.remove(&name);
            names.modules.remove(&name);
        }
    }
    names
}

fn collect_use(item: &ItemUse, names: &mut Names, parent: &Names) {
    fn walk(tree: &UseTree, prefix: &mut Vec<String>, names: &mut Names, parent: &Names) {
        match tree {
            UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                walk(&path.tree, prefix, names, parent);
                prefix.pop();
            }
            UseTree::Name(name) => insert(prefix, &name.ident.to_string(), None, names, parent),
            UseTree::Rename(name) => insert(
                prefix,
                &name.ident.to_string(),
                Some(name.rename.to_string()),
                names,
                parent,
            ),
            UseTree::Group(group) => {
                for tree in &group.items {
                    walk(tree, prefix, names, parent);
                }
            }
            UseTree::Glob(_) if matches!(prefix.as_slice(), [root, process] if (root == "std" || root == "tokio") && process == "process") =>
            {
                names.commands.insert("Command".into());
            }
            UseTree::Glob(_) if matches!(prefix.as_slice(), [scope] if scope == "super") => {
                names.commands.extend(parent.commands.iter().cloned());
                names.modules.extend(parent.modules.iter().cloned());
            }
            UseTree::Glob(_) => {}
        }
    }
    fn insert(
        prefix: &[String],
        name: &str,
        rename: Option<String>,
        names: &mut Names,
        parent: &Names,
    ) {
        let mut full = prefix.to_vec();
        if name != "self" {
            full.push(name.into());
        }
        let alias = rename.unwrap_or_else(|| full.last().cloned().unwrap_or_default());
        match full.as_slice() {
            [scope, original] if scope == "super" && parent.commands.contains(original) => {
                names.commands.insert(alias);
            }
            [scope, original] if scope == "super" && parent.modules.contains(original) => {
                names.modules.insert(alias);
            }
            [scope, module, command]
                if scope == "super" && parent.modules.contains(module) && command == "Command" =>
            {
                names.commands.insert(alias);
            }
            [root, process, command]
                if (root == "std" || root == "tokio")
                    && process == "process"
                    && command == "Command" =>
            {
                names.commands.insert(alias);
            }
            [root, process] if (root == "std" || root == "tokio") && process == "process" => {
                names.modules.insert(alias);
            }
            _ => {}
        }
    }
    walk(&item.tree, &mut Vec::new(), names, parent);
}

fn is_command_constructor(path: &syn::Path, names: &Names, parent: &Names) -> bool {
    let parts: Vec<_> = path
        .segments
        .iter()
        .map(|part| part.ident.to_string())
        .collect();
    match parts.as_slice() {
        [root, process, command, new] => {
            ((root == "std" || root == "tokio") && process == "process"
                || root == "super" && parent.modules.contains(process))
                && command == "Command"
                && new == "new"
        }
        [module, command, new] => {
            (names.modules.contains(module) && command == "Command" && new == "new")
                || (module == "super" && parent.commands.contains(command) && new == "new")
        }
        [command, new] => names.commands.contains(command) && new == "new",
        _ => false,
    }
}
