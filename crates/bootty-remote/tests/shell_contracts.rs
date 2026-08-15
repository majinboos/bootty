use bootty_remote::shell_quote;

#[test]
fn remote_shell_arguments_are_single_quoted() {
    assert_eq!(shell_quote("foo'bar"), "'foo'\\''bar'");
}
