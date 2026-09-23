//! Rename-retry support.
//!
//! frps answers a duplicate proxy name with `proxy [x] already exists`. Rather
//! than retrying the same doomed name forever, the client can walk a fixed
//! sequence of prefixes: `ex_1_`, `ex_2_`, `ex_3_`, then back to `ex_1_`. The
//! sequence is deterministic so that a monitoring plugin which aggregates
//! proxy names does not accumulate random identities.
//!
//! This is off by default, because it changes the public proxy name.

/// How many prefixed variants are tried before wrapping back to `ex_1_`.
pub const MAX_EX_SEQUENCE: u32 = 3;

pub fn ex_prefix(number: u32) -> String {
    format!("ex_{number}_")
}

/// Advances the sequence, wrapping at [`MAX_EX_SEQUENCE`].
pub fn next_ex(number: u32) -> u32 {
    if number >= MAX_EX_SEQUENCE {
        1
    } else {
        number + 1
    }
}

/// Rewrites the naming flags of a generated command.
///
/// Run against the *original* command every time so prefixes never stack.
/// Only the flags that make up the proxy's public identity are touched.
pub fn apply_rename_prefix(command: &str, prefix: &str) -> String {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let mut result: Vec<String> = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        result.push(token.to_string());
        if matches!(token, "--proxy-name" | "--sd" | "--custom-domain") {
            if let Some(value) = tokens.get(index + 1) {
                index += 1;
                if token == "--custom-domain" {
                    let renamed = value
                        .split(',')
                        .map(|item| format!("{prefix}{}", strip_rename_prefix(item)))
                        .collect::<Vec<_>>()
                        .join(",");
                    result.push(renamed);
                } else {
                    result.push(format!("{prefix}{}", strip_rename_prefix(value)));
                }
            }
        }
        index += 1;
    }
    result.join(" ")
}

/// Peels any previously applied prefix so that a rename is idempotent.
///
/// Only meaningful while renaming is enabled; callers must not apply this to a
/// proxy name the user chose themselves, or a name that genuinely starts with
/// `ex_1_` would be mangled.
pub fn strip_rename_prefix(value: &str) -> String {
    let mut current = value;
    loop {
        let stripped = strip_one(current);
        match stripped {
            Some(next) => current = next,
            None => return current.to_string(),
        }
    }
}

fn strip_one(value: &str) -> Option<&str> {
    let rest = value.strip_prefix("ex_")?;
    let (digits, rest) = rest.split_once('_')?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_the_sequence_and_wraps() {
        assert_eq!(ex_prefix(1), "ex_1_");
        assert_eq!(next_ex(1), 2);
        assert_eq!(next_ex(MAX_EX_SEQUENCE), 1);
    }

    #[test]
    fn prefixes_the_proxy_name_in_place() {
        assert_eq!(
            apply_rename_prefix("tcp --proxy-name demo --remote-port 8080", "ex_1_"),
            "tcp --proxy-name ex_1_demo --remote-port 8080"
        );
    }

    #[test]
    fn prefixes_every_custom_domain_and_the_subdomain() {
        assert_eq!(
            apply_rename_prefix(
                "http --proxy-name web --custom-domain a.com,b.com --sd web",
                "ex_2_"
            ),
            "http --proxy-name ex_2_web --custom-domain ex_2_a.com,ex_2_b.com --sd ex_2_web"
        );
    }

    #[test]
    fn renaming_is_idempotent() {
        let original = "tcp --proxy-name demo --remote-port 1";
        let once = apply_rename_prefix(original, "ex_1_");
        let twice = apply_rename_prefix(&once, "ex_1_");
        assert_eq!(once, twice);
    }

    #[test]
    fn switching_prefixes_does_not_stack_them() {
        let original = "tcp --proxy-name demo --remote-port 1";
        let first = apply_rename_prefix(original, "ex_1_");
        let second = apply_rename_prefix(&first, "ex_2_");
        assert_eq!(second, "tcp --proxy-name ex_2_demo --remote-port 1");
    }

    #[test]
    fn strips_only_well_formed_prefixes() {
        assert_eq!(strip_rename_prefix("ex_1_demo"), "demo");
        assert_eq!(strip_rename_prefix("ex_1_ex_2_demo"), "demo");
        assert_eq!(strip_rename_prefix("ex_demo"), "ex_demo");
        assert_eq!(strip_rename_prefix("ex__demo"), "ex__demo");
        assert_eq!(strip_rename_prefix("demo"), "demo");
    }
}
