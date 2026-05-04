#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![deny(clippy::nursery)]
#![deny(clippy::cargo)]
#![allow(
    clippy::multiple_crate_versions,
    reason = "transitive dependencies from russh/poem bring duplicate versions we cannot control"
)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::dbg_macro)]
#![deny(clippy::exit)]
#![deny(clippy::mem_forget)]
#![deny(clippy::infinite_loop)]
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]
#![deny(clippy::wildcard_enum_match_arm)]
#![deny(clippy::as_conversions)]
#![deny(clippy::clone_on_ref_ptr)]
#![deny(clippy::map_err_ignore)]
#![deny(clippy::implicit_clone)]
#![deny(clippy::if_then_some_else_none)]
#![deny(clippy::rc_mutex)]
#![deny(clippy::empty_structs_with_brackets)]
#![deny(clippy::redundant_type_annotations)]
#![deny(clippy::same_name_method)]
#![deny(clippy::tests_outside_test_module)]
#![deny(clippy::large_include_file)]
#![deny(clippy::renamed_function_params)]
#![deny(clippy::rest_pat_in_fully_bound_structs)]
#![deny(clippy::ref_patterns)]
#![deny(clippy::format_push_string)]
#![deny(clippy::string_lit_as_bytes)]
#![deny(clippy::unseparated_literal_suffix)]
#![deny(clippy::semicolon_inside_block)]
#![deny(clippy::needless_raw_strings)]
#![deny(clippy::needless_raw_string_hashes)]
#![deny(clippy::missing_asserts_for_indexing)]
#![deny(clippy::pub_use)]
#![deny(clippy::absolute_paths)]
#![deny(clippy::allow_attributes_without_reason)]

pub mod adapters;
pub mod application;
pub mod composition;
pub mod domain;
pub mod embed;
pub mod infra;
pub mod ports;
