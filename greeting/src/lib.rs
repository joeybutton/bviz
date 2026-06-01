/// Build the greeting shown by the `bazel_viz` placeholder binary.
pub fn greeting(name: &str) -> String {
    format!("Hello, {name}! bazel_viz is wired up correctly.")
}

#[cfg(test)]
mod tests {
    use super::greeting;

    #[test]
    fn greets_by_name() {
        assert_eq!(
            greeting("world"),
            "Hello, world! bazel_viz is wired up correctly."
        );
    }
}
