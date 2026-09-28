//! `--flag value`, `--flag=value`, and bare `--flag` parsing shared by the commands.

/// The value of `--name value` or `--name=value`.
pub(crate) fn flag_value(args: &[String], name: &str) -> Option<String> {
    let long = format!("--{name}");
    let long_eq = format!("--{name}=");
    let mut index = 0;
    while index < args.len() {
        if args[index] == long {
            return args.get(index + 1).cloned();
        }
        if let Some(value) = args[index].strip_prefix(&long_eq) {
            return Some(value.to_owned());
        }
        index += 1;
    }
    None
}

/// Whether bare `--name` is present.
pub(crate) fn has_flag(args: &[String], name: &str) -> bool {
    let long = format!("--{name}");
    args.iter().any(|argument| argument == &long)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn flags_are_read_in_both_forms() {
        let args = strings(&["--out", "/tmp/a", "--force", "--state-dir=/s"]);
        assert_eq!(flag_value(&args, "out").as_deref(), Some("/tmp/a"));
        assert_eq!(flag_value(&args, "state-dir").as_deref(), Some("/s"));
        assert!(has_flag(&args, "force"));
        assert!(!has_flag(&args, "start"));
        assert_eq!(flag_value(&args, "missing"), None);
    }
}
