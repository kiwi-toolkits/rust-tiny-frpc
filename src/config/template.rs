//! `{{ .Envs.NAME }}` substitution.
//!
//! Applied to the whole file as text before parsing, matching the Go original.
//! A missing environment variable renders as the empty string rather than
//! failing, which is what `text/template` does for a missing map key.

use std::env;

/// Replaces every `{{ .Envs.NAME }}` action. Any other action is rejected
/// instead of being passed through, because frp's own template engine supports
/// far more (ranges, conditionals, other fields) and silently leaving those in
/// place would produce a config that looks valid but is not.
pub fn render(input: &str) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| "template: unterminated action".to_string())?;
        let action = after[..end].trim();
        let name = action
            .strip_prefix(".Envs.")
            .ok_or_else(|| format!("template: unsupported action {{{{{action}}}}}"))?;
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("template: unsupported action {{{{{action}}}}}"));
        }
        out.push_str(&env::var(name).unwrap_or_default());
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_environment_variables() {
        env::set_var("RUST_TINY_FRPC_TEST_HOST", "example.com");
        let rendered = render("serverAddr = \"{{ .Envs.RUST_TINY_FRPC_TEST_HOST }}\"").unwrap();
        assert_eq!(rendered, "serverAddr = \"example.com\"");
    }

    #[test]
    fn missing_variables_become_empty() {
        env::remove_var("RUST_TINY_FRPC_TEST_MISSING");
        assert_eq!(
            render("[{{ .Envs.RUST_TINY_FRPC_TEST_MISSING }}]").unwrap(),
            "[]"
        );
    }

    #[test]
    fn rejects_actions_we_do_not_support() {
        assert!(render("{{ .Name }}").is_err());
        assert!(render("{{ .Envs. }").is_err());
        assert!(render("{{ .Envs.A }} and {{ .Envs.B }").is_err());
    }

    #[test]
    fn leaves_plain_text_alone() {
        assert_eq!(render("no actions here").unwrap(), "no actions here");
    }
}
