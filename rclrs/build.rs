use std::{
    env,
    path::{Path, PathBuf},
};
const AMENT_PREFIX_PATH: &str = "AMENT_PREFIX_PATH";
const ROS_DISTRO: &str = "ROS_DISTRO";

/// Distributions with checked-in bindings (`src/rcl_bindings_generated_<d>.rs`)
/// that are used as is. Any other distribution, Rolling included since it
/// keeps changing, gets bindings generated from its own headers at build
/// time. `RCLRS_GENERATE_BINDINGS=1` forces generation for these too.
const CHECKED_IN_DISTROS: &[&str] = &["humble", "jazzy", "kilted"];

fn get_env_var_or_abort(env_var: &'static str) -> String {
    if let Ok(value) = env::var(env_var) {
        value
    } else {
        panic!(
            "{} environment variable not set - please source ROS 2 installation first.",
            env_var
        );
    }
}

fn main() {
    // Any distribution name: a release newer than this crate is still built.
    println!("cargo:rustc-check-cfg=cfg(ros_distro, values(any()))");
    println!("cargo:rustc-check-cfg=cfg(rclrs_bindings_generated)");
    println!("cargo:rustc-check-cfg=cfg(rmw_liveliness_manual_by_node)");
    println!("cargo:rerun-if-env-changed={ROS_DISTRO}");
    println!("cargo:rerun-if-env-changed={AMENT_PREFIX_PATH}");
    println!("cargo:rerun-if-env-changed=RCLRS_GENERATE_BINDINGS");
    println!("cargo:rerun-if-changed=src/rcl_wrapper.h");
    let ros_distro = if let Ok(value) = env::var(ROS_DISTRO) {
        value
    } else {
        cfg_if::cfg_if! {
            if #[cfg(feature="use_ros_shim")] {
                use rustflags;
                // // Look for --cfg ros_distro=<ros_distro>
                for flag in rustflags::from_env() {
                    if matches!(flag, rustflags::Flag::Cfg { ref name, value : _ } if name == "ros_distro") {
                        if let rustflags::Flag::Cfg {name:_, value: flag_value} = flag {
                            println!("cargo:rustc-cfg=ros_distro=\"{}\"", flag_value.unwrap());
                            return;
                        } else {
                            continue;
                        }
                    }
                }
                let error_msg =
                    "When using the use_ros_shim feature, you must pass the ROS distribution you are targeting as a compiler flag with --cfg ros_distro=\"<ros_distro>\"";
                panic!("{}", error_msg);
            } else {
                let error_msg =
                    "ROS_DISTRO environment variable not set - please source ROS 2 installation first.";
                panic!("{}", error_msg);
            }
        }
    };
    println!("cargo:rustc-cfg=ros_distro=\"{ros_distro}\"");

    let ament_prefix_paths = get_env_var_or_abort(AMENT_PREFIX_PATH);

    let force = env::var("RCLRS_GENERATE_BINDINGS").is_ok_and(|v| v == "1");
    let bindings = if force || !CHECKED_IN_DISTROS.contains(&ros_distro.as_str()) {
        println!("cargo:rustc-cfg=rclrs_bindings_generated");
        generate_bindings(&ament_prefix_paths)
    } else {
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        std::fs::read_to_string(
            manifest_dir.join(format!("src/rcl_bindings_generated_{ros_distro}.rs")),
        )
        .unwrap_or_default()
    };
    // Items a distribution may drop. Lyrical removed the deprecated
    // MANUAL_BY_NODE liveliness policy.
    if bindings.contains("RMW_QOS_POLICY_LIVELINESS_MANUAL_BY_NODE") {
        println!("cargo:rustc-cfg=rmw_liveliness_manual_by_node");
    }
    for ament_prefix_path in ament_prefix_paths.split(':').map(Path::new) {
        // Link the native libraries
        let library_path = ament_prefix_path.join("lib");
        println!("cargo:rustc-link-search=native={}", library_path.display());
    }

    println!("cargo:rustc-link-lib=dylib=rcl");
    println!("cargo:rustc-link-lib=dylib=rcl_action");
    println!("cargo:rustc-link-lib=dylib=rcl_yaml_param_parser");
    println!("cargo:rustc-link-lib=dylib=rcutils");
    println!("cargo:rustc-link-lib=dylib=rmw");
    println!("cargo:rustc-link-lib=dylib=rmw_implementation");
}

/// Run bindgen on `src/rcl_wrapper.h` against the sourced distribution's
/// headers, with the options of `generate_bindings.py` (which writes the
/// checked-in files), into `$OUT_DIR/rcl_bindings.rs`. Returns the bindings.
fn generate_bindings(ament_prefix_paths: &str) -> String {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("rcl_bindings.rs");

    // -isystem <prefix>/include/<package> for every package in the ament
    // index, as the script does. Only registered packages: a prefix's
    // include/ also holds other directories (e.g. Cyclone's idl/) that must
    // not shadow the real headers.
    let mut clang_args = Vec::new();
    for prefix in ament_prefix_paths.split(':').filter(|p| !p.is_empty()) {
        let prefix = Path::new(prefix);
        let index = prefix.join("share/ament_index/resource_index/packages");
        let Ok(entries) = std::fs::read_dir(index) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| prefix.join("include").join(e.file_name()))
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            clang_args.push("-isystem".to_string());
            clang_args.push(dir.display().to_string());
        }
    }

    let mut builder = bindgen::Builder::default()
        .header(manifest_dir.join("src/rcl_wrapper.h").display().to_string())
        .clang_args(clang_args)
        .derive_copy(false)
        .layout_tests(false)
        .default_enum_style(bindgen::EnumVariation::Rust {
            non_exhaustive: false,
        });
    for prefix in ["rcl_", "rmw_", "rcutils_", "rosidl_"] {
        let pattern = format!("{prefix}.*");
        builder = builder
            .allowlist_type(&pattern)
            .allowlist_function(&pattern)
            .allowlist_var(&pattern);
    }
    // Declared by the vendored message crates already (rosidl_generator_rs
    // generates them with a *const c_void return type).
    for kind in ["message", "service", "action"] {
        builder = builder.blocklist_function(format!(
            "rosidl_typesupport_.*__get_{kind}_type_support_handle__.*"
        ));
    }
    let bindings = builder
        .generate()
        .unwrap_or_else(|e| {
            panic!("rclrs: generating the rcl bindings for this ROS distribution failed: {e}")
        })
        .to_string();
    std::fs::write(&out, &bindings).expect("rclrs: writing the generated rcl bindings failed");
    bindings
}
