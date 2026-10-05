use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use oxc_diagnostics::OxcDiagnostic;
use oxc_macros::declare_oxc_lint;
use oxc_span::Span;

use crate::{context::LintContext, rule::Rule};

fn no_relative_packages_diagnostic(
    span: Span,
    proper_import: &str,
    import_path: &str,
) -> OxcDiagnostic {
    OxcDiagnostic::warn("Relative import from another package is not allowed")
        .with_help(format!("Use `{proper_import}` instead of `{import_path}`"))
        .with_label(span)
}

#[derive(Debug, Default, Clone)]
pub struct NoRelativePackages;

// <https://github.com/import-js/eslint-plugin-import/blob/v2.32.0/docs/rules/no-relative-packages.md>
declare_oxc_lint!(
    /// ### What it does
    ///
    /// Forbids importing modules from another package using relative paths.
    /// A package is identified by the closest `package.json` with a `name` field.
    ///
    /// ### Why is this bad?
    ///
    /// Reaching into a sibling package with a relative path (e.g. in a monorepo)
    /// bypasses the package boundary: the import keeps working locally but breaks
    /// once the packages are published or moved, and it ignores the target package's
    /// `main`/`exports` entry points. Packages should be imported by their name.
    ///
    /// ### Examples
    ///
    /// Given the following folder structure:
    /// ```
    /// my-project
    /// ├── packages
    /// │   ├── foo
    /// │   │   ├── index.js
    /// │   │   └── package.json  // { "name": "foo" }
    /// │   └── bar
    /// │       ├── index.js
    /// │       └── package.json  // { "name": "bar" }
    /// └── entry.js
    /// ```
    ///
    /// Examples of **incorrect** code for this rule:
    /// ```js
    /// // packages/foo/index.js
    /// import bar from '../bar';
    /// import bar from '../bar/index.js';
    /// ```
    ///
    /// Examples of **correct** code for this rule:
    /// ```js
    /// // packages/foo/index.js
    /// import bar from 'bar';
    /// import baz from './baz';
    /// ```
    ///
    /// ### Limitations
    ///
    /// Only static ES module imports and re-exports that can be resolved are checked.
    /// CommonJS `require` calls and dynamic `import()` expressions are not checked.
    NoRelativePackages,
    import,
    suspicious,
    fix,
    version = "next",
    short_description = "Forbid importing packages through relative paths.",
);

impl Rule for NoRelativePackages {
    fn run_once(&self, ctx: &LintContext<'_>) {
        let module_record = ctx.module_record();
        let Some(context_pkg) = find_named_package(&module_record.resolved_absolute_path) else {
            return;
        };

        for (import_path, requested_modules) in &module_record.requested_modules {
            let import_path = import_path.as_str();
            if !is_relative(import_path) {
                continue;
            }
            let Some(remote_module_record) = module_record.get_loaded_module(import_path) else {
                continue;
            };
            let resolved_import = &remote_module_record.resolved_absolute_path;
            let Some(import_pkg) = find_named_package(resolved_import) else {
                continue;
            };
            if import_pkg.name == context_pkg.name {
                continue;
            }
            let Some(proper_import) = proper_import(import_path, &import_pkg, resolved_import)
            else {
                continue;
            };

            for requested_module in requested_modules {
                let span = requested_module.span;
                ctx.diagnostic_with_fix(
                    no_relative_packages_diagnostic(span, &proper_import, import_path),
                    |fixer| {
                        let quote = fixer.source_range(span).chars().next().unwrap_or('"');
                        fixer.replace(span, format!("{quote}{proper_import}{quote}"))
                    },
                );
            }
        }
    }
}

/// Matches `parent`, `sibling` and `index` import types of `eslint-plugin-import`.
fn is_relative(import_path: &str) -> bool {
    import_path == "."
        || import_path == ".."
        || import_path.starts_with("./")
        || import_path.starts_with("../")
}

struct NamedPackage {
    name: String,
    /// Directory containing the `package.json`.
    root: PathBuf,
}

/// Find the closest `package.json` with a non-empty `name` field, starting from the directory
/// of `file_path`. Like `eslint-plugin-import`, a `package.json` without a name is skipped and
/// the search continues from the parent of its directory.
fn find_named_package(file_path: &Path) -> Option<NamedPackage> {
    for dir in file_path.parent()?.ancestors() {
        let Ok(content) = fs::read_to_string(dir.join("package.json")) else {
            continue;
        };
        let name = serde_json::from_str::<serde_json::Value>(&content).ok().and_then(|json| {
            json.get("name").and_then(serde_json::Value::as_str).map(ToString::to_string)
        });
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            return Some(NamedPackage { name, root: dir.to_path_buf() });
        }
    }
    None
}

/// Build the package-based specifier that should replace `import_path`, e.g.
/// `../package-named/lib/foo` -> `package-named/lib/foo`.
fn proper_import(import_path: &str, import_pkg: &NamedPackage, resolved: &Path) -> Option<String> {
    let proper_path = resolved.strip_prefix(&import_pkg.root).ok()?;
    let import_base_name = import_path.rsplit('/').find(|segment| !segment.is_empty())?;
    let root_base_name = import_pkg.root.file_name().and_then(|name| name.to_str());

    let mut segments: Vec<&str> = import_pkg.name.split('/').collect();
    for component in proper_path.parent().into_iter().flat_map(Path::components) {
        match component {
            Component::Normal(segment) => push_segment(&mut segments, segment.to_str()?),
            Component::ParentDir => push_segment(&mut segments, ".."),
            _ => {}
        }
    }
    if root_base_name != Some(import_base_name) {
        push_segment(&mut segments, import_base_name);
    }
    Some(segments.join("/"))
}

/// Push a path segment, normalizing `.` and `..` like Node's `path.join`.
fn push_segment<'a>(segments: &mut Vec<&'a str>, segment: &'a str) {
    match segment {
        "" | "." => {}
        ".." => {
            segments.pop();
        }
        segment => segments.push(segment),
    }
}

#[test]
fn test() {
    use crate::tester::Tester;

    {
        let pass = vec![
            r#"import foo from "./index.js""#,
            r#"import bar from "../bar""#,
            r#"const bar = require("../bar.js")"#,
            r#"const bar = require("../not/a/file/path.js")"#,
            r#"import "package""#,
            r#"require("../bar.js")"#,
            r#"import foo from "../not/a/file/path.js""#,
        ];

        let fail = vec![
            r#"import foo from "../package-named""#,
            r#"import foo from "../package-scoped""#,
            r#"import foo from "../package-named/index.js""#,
            r#"export { default } from "../package-named""#,
            r#"export * from "../package-scoped/index""#,
        ];

        let fix = vec![
            (r#"import foo from "../package-named""#, r#"import foo from "package-named""#),
            (r"import foo from '../package-named'", r"import foo from 'package-named'"),
            (r#"import foo from "../package-scoped""#, r#"import foo from "@scope/package-named""#),
            (
                r#"import foo from "../package-named/index.js""#,
                r#"import foo from "package-named/index.js""#,
            ),
            (
                r#"export * from "../package-scoped/index""#,
                r#"export * from "@scope/package-named/index""#,
            ),
        ];

        Tester::new(NoRelativePackages::NAME, NoRelativePackages::PLUGIN, pass, fail)
            .with_import_plugin(true)
            .change_rule_path("package/index.js")
            .expect_fix(fix)
            .test_and_snapshot();
    }

    {
        let pass = vec![r#"import {foo} from "a""#, r#"import foo from "./index.js""#];
        let fail = vec![r#"import bar from "../bar""#];
        let fix = vec![(
            r#"import bar from "../bar""#,
            r#"import bar from "oxc/crates/oxc_linter/fixtures/import/bar""#,
        )];

        Tester::new(NoRelativePackages::NAME, NoRelativePackages::PLUGIN, pass, fail)
            .with_import_plugin(true)
            .change_rule_path("package-named/index.js")
            .expect_fix(fix)
            .with_snapshot_suffix("package-named")
            .test_and_snapshot();
    }

    {
        let pass = vec![r#"import bar from "./bar""#];
        let fail = vec![r#"import foo from "./package-named""#];
        let fix =
            vec![(r#"import foo from "./package-named""#, r#"import foo from "package-named""#)];

        Tester::new(NoRelativePackages::NAME, NoRelativePackages::PLUGIN, pass, fail)
            .with_import_plugin(true)
            .change_rule_path("bar.js")
            .expect_fix(fix)
            .with_snapshot_suffix("bar")
            .test();
    }
}
