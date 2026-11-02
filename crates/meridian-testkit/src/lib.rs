//! Helpers for tests that need no real game: a scripted client and backend that
//! talk over in-memory pipes, and a recorder that keeps the ordered packet ids and
//! bytes of a session so that two sessions can be compared.
