use super::super::*;

pub(crate) fn one_file(src: &str) -> (State, &'static str) {
    let uri = "file:///t.aly";
    let mut st = State {
        root: Some(PathBuf::from("/")),
        mirror: PathBuf::from("/m"),
        snippets: true,
        ..State::default()
    };
    st.docs.insert(
        uri.to_string(),
        Doc::new(
            src.to_string(),
            1,
            &EmitOptions::default(),
            &alloy::luaux::Config::default(),
            None,
        ),
    );

    (st, uri)
}
