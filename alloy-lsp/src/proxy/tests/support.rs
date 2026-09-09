use super::super::*;

/// A workspace of several files, each `(uri, source)`. The state has
/// no project on disk, so a test that needs the globals of a project
/// passes `in_project` through the options here.
pub(crate) fn files(sources: &[(&str, &str)]) -> State {
    let mut st = State {
        root: Some(PathBuf::from("/")),
        mirror: PathBuf::from("/m"),
        snippets: true,
        ..State::default()
    };

    for (uri, src) in sources {
        let options = EmitOptions {
            file_name: uri.trim_start_matches("file://").to_string(),
            in_project: true,
            ..EmitOptions::default()
        };
        st.docs.insert(
            (*uri).to_string(),
            Doc::new(
                (*src).to_string(),
                1,
                &options,
                &alloy::luaux::Config::default(),
                None,
            ),
        );
    }

    st
}

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
