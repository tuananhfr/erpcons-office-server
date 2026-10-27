/// Whether `err` is a unique violation of `constraint` (an index name for unique indexes).
pub fn is_unique_violation(err: &sqlx::Error, constraint: &str) -> bool {
    match err {
        sqlx::Error::Database(e) => {
            e.code().as_deref() == Some("23505") && e.constraint() == Some(constraint)
        }
        _ => false,
    }
}
