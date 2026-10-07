use crate::{Dialect, FEATURES, FeatureSeverity, Support, Target, Version, check_features, parse};

fn found(text: &str, dialect: Dialect, version: Option<Version>) -> Vec<(&'static str, FeatureSeverity)> {
    let parsed = parse(text, dialect);
    check_features(&parsed.syntax(), Target::new(dialect, version))
        .into_iter()
        .map(|finding| (finding.feature, finding.severity))
        .collect()
}

fn reports(text: &str, dialect: Dialect, version: Option<Version>, id: &str) -> Option<FeatureSeverity> {
    found(text, dialect, version)
        .into_iter()
        .find(|(feature, _)| *feature == id)
        .map(|(_, severity)| severity)
}

#[test]
fn ids_are_unique_and_every_row_finds_its_example() {
    let mut ids: Vec<&str> = FEATURES.iter().map(|feature| feature.id).collect();
    ids.sort();
    let count = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), count, "feature ids are unique");
    for feature in FEATURES {
        let detected = Dialect::DATABASES.iter().chain([&Dialect::Generic]).any(|dialect| {
            let parsed = parse(feature.example, *dialect);
            let root = parsed.syntax();
            root.descendants_with_tokens()
                .filter(|element| feature.kinds.contains(&element.kind()))
                .any(|element| (feature.detect)(&element).is_some())
        });
        assert!(detected, "{}: the example is not detected in any dialect", feature.id);
    }
}

#[test]
fn every_row_holds_against_its_example_in_every_dialect() {
    for feature in FEATURES {
        for dialect in Dialect::DATABASES {
            let parsed = parse(feature.example, dialect);
            let errors: Vec<String> = parsed.errors().iter().map(|error| error.message.clone()).collect();
            let at = |version: Version| reports(feature.example, dialect, Some(version), feature.id);
            let context = format!("{} in {dialect}: {:?}", feature.id, feature.example);
            match feature.support_in(dialect).expect("a database") {
                Support::Always => {
                    assert!(errors.is_empty(), "{context} parses: {errors:?}");
                    assert_eq!(at(dialect.minimum()), None, "{context} is accepted");
                    assert_eq!(
                        reports(feature.example, dialect, None, feature.id),
                        None,
                        "{context} is accepted"
                    );
                }
                Support::Since(version) => {
                    assert!(errors.is_empty(), "{context} parses: {errors:?}");
                    assert!(
                        version > dialect.minimum(),
                        "{context}: a version at the minimum is Always"
                    );
                    assert_eq!(
                        at(version.previous()),
                        Some(FeatureSeverity::Error),
                        "{context} before {version}"
                    );
                    assert_eq!(at(version), None, "{context} at {version}");
                }
                Support::DeprecatedSince(version) => {
                    assert!(errors.is_empty(), "{context} parses: {errors:?}");
                    if version > dialect.minimum() {
                        assert_eq!(at(version.previous()), None, "{context} before {version}");
                    }
                    assert_eq!(at(version), Some(FeatureSeverity::Warning), "{context} at {version}");
                }
                Support::Never => {
                    // The text may mean something else in the dialect, as `@total` is a parameter in
                    // SQLite: then the construct is not there to reject.
                    let root = parsed.syntax();
                    let present = root
                        .descendants_with_tokens()
                        .any(|element| feature.kinds.contains(&element.kind()));
                    let rejected = !errors.is_empty() || reports(feature.example, dialect, None, feature.id).is_some();
                    assert!(rejected || !present, "{context} is rejected");
                }
            }
        }
        let everywhere_never = feature.support.iter().all(|support| *support == Support::Never);
        let generic = reports(feature.example, Dialect::Generic, None, feature.id);
        if everywhere_never {
            assert_eq!(
                generic,
                Some(FeatureSeverity::Error),
                "{} is rejected without a dialect",
                feature.id
            );
        } else {
            assert_eq!(generic, None, "{} is accepted without a dialect", feature.id);
        }
    }
}

#[test]
fn messages_name_the_dialect_and_the_version() {
    let parsed = parse("SELECT a FROM t INTERSECT SELECT a FROM u", Dialect::Mysql);
    let found = check_features(&parsed.syntax(), Target::new(Dialect::Mysql, Version::parse("8.0.30")));
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].message,
        "INTERSECT and EXCEPT are only available since MySQL 8.0.31"
    );
    let parsed = parse(
        "MERGE INTO t USING s ON t.a = s.a WHEN MATCHED THEN DELETE",
        Dialect::Sqlite,
    );
    let found = check_features(&parsed.syntax(), Target::new(Dialect::Sqlite, None));
    assert_eq!(found[0].message, "MERGE is not supported by SQLite");
    assert_eq!(found[0].range, crate::TextRange::new(0.into(), 5.into()));
    let parsed = parse("SELECT a FROM t QUALIFY a > 1", Dialect::Generic);
    let found = check_features(&parsed.syntax(), Target::GENERIC);
    assert_eq!(
        found[0].message,
        "QUALIFY is not supported by SQLite, MySQL, MariaDB or PostgreSQL"
    );
    let parsed = parse("CREATE TABLE t (a INT(11) ZEROFILL)", Dialect::Mysql);
    let found = check_features(&parsed.syntax(), Target::new(Dialect::Mysql, Version::parse("8.4")));
    let messages: Vec<&str> = found.iter().map(|finding| finding.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "A display width of an integer type is deprecated since MySQL 8.0.17",
            "ZEROFILL is deprecated since MySQL 8.0.17"
        ]
    );
    assert!(found.iter().all(|finding| finding.deprecated));
}

#[test]
fn the_same_script_is_judged_per_dialect() {
    let text = "SELECT `a` FROM t LIMIT 1, 2;\nINSERT INTO t (a) VALUES (1) RETURNING a;";
    let ids = |dialect| -> Vec<&'static str> { found(text, dialect, None).into_iter().map(|(id, _)| id).collect() };
    assert_eq!(ids(Dialect::Postgres), ["backtick-identifiers", "limit-with-comma"]);
    assert_eq!(ids(Dialect::Mysql), ["insert-returning"]);
    assert!(ids(Dialect::Mariadb).is_empty());
    assert!(ids(Dialect::Sqlite).is_empty());
    assert!(ids(Dialect::Generic).is_empty());
}
