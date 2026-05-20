pub mod astgrep_go;
pub mod astgrep_hcl;
pub mod astgrep_py;
pub mod astgrep_rust;
pub mod astgrep_ts;
pub mod dispatch;
pub mod fingerprint;
pub mod fingerprint_go;
pub mod fingerprint_python;
pub mod fingerprint_rust;
pub mod fingerprint_typescript;
pub mod markdown;

#[cfg(test)]
mod astgrep_go_tests;
#[cfg(test)]
mod astgrep_hcl_tests;
#[cfg(test)]
mod astgrep_py_tests;
#[cfg(test)]
mod astgrep_rust_tests;
#[cfg(test)]
mod astgrep_ts_tests;
#[cfg(test)]
mod dispatch_tests;
#[cfg(test)]
mod fingerprint_go_tests;
#[cfg(test)]
mod fingerprint_python_tests;
#[cfg(test)]
mod fingerprint_rust_tests;
#[cfg(test)]
mod fingerprint_tests;
#[cfg(test)]
mod fingerprint_typescript_tests;
#[cfg(test)]
mod markdown_tests;
