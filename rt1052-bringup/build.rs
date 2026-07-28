use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(mut command: Command, description: &str) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("failed to run {description}: {error}"));
    assert!(status.success(), "{description} failed with {status}");
}

fn compile_nxp_host(manifest: &Path) {
    let vendor = manifest
        .parent()
        .expect("bring-up crate must be inside the repository")
        .join(".vendor");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let device = vendor.join("mcux-devices-rt/RT1050/MIMXRT1052");
    let core = vendor.join("mcuxsdk-core");
    let usb = vendor.join("mcux-sdk-middleware-usb");
    let cmsis = vendor.join("mcu-sdk-cmsis");

    let includes = [
        manifest.join("csrc"),
        device.clone(),
        vendor.join("mcux-devices-rt/RT1050/periph"),
        device.join("drivers"),
        core.join("drivers/common"),
        core.join("drivers/cache/armv7-m7"),
        cmsis.join("Core/Include"),
        usb.join("include"),
        usb.join("host"),
        usb.join("host/class"),
        usb.join("phy"),
    ];
    let sources = [
        manifest.join("csrc/osa_baremetal.c"),
        manifest.join("csrc/nxp_host_ffi.c"),
        core.join("drivers/common/fsl_common.c"),
        device.join("drivers/fsl_clock.c"),
        device.join("system_MIMXRT1052.c"),
        usb.join("phy/usb_phy.c"),
        usb.join("host/usb_host_hci.c"),
        usb.join("host/usb_host_devices.c"),
        usb.join("host/usb_host_framework.c"),
        usb.join("host/usb_host_ehci.c"),
        usb.join("host/class/usb_host_hub.c"),
        usb.join("host/class/usb_host_hub_app.c"),
        usb.join("host/class/usb_host_hid.c"),
    ];

    for path in includes.iter().chain(sources.iter()) {
        assert!(path.exists(), "missing NXP SDK path: {}", path.display());
    }

    let mut objects = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let stem = source
            .file_stem()
            .expect("C source has a file stem")
            .to_string_lossy();
        let object = out.join(format!("{index:02}_{stem}.o"));
        let mut command = Command::new("arm-none-eabi-gcc");
        command.args([
            "-c",
            "-mcpu=cortex-m7",
            "-mthumb",
            "-mfpu=fpv5-d16",
            "-mfloat-abi=hard",
            "-std=gnu11",
            "-Os",
            "-ffunction-sections",
            "-fdata-sections",
            "-fno-builtin",
            "-Wall",
            "-DCPU_MIMXRT1052CVL5B",
            "-DSDK_DEBUGCONSOLE=2",
            "-DDATA_SECTION_IS_CACHEABLE=0",
        ]);
        for include in &includes {
            command.arg("-I").arg(include);
        }
        command.arg(source).arg("-o").arg(&object);
        run(command, &format!("compile {}", source.display()));
        objects.push(object);
    }

    let archive = out.join("libnxp_usb_host.a");
    let mut command = Command::new("arm-none-eabi-ar");
    command.arg("crs").arg(&archive).args(&objects);
    run(command, "archive NXP USB Host objects");

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=nxp_usb_host");
    println!("cargo:rerun-if-changed={}", manifest.join("csrc").display());
}

fn main() {
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"));
    println!("cargo:rustc-link-search={}", env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rerun-if-changed=memory.x");

    if env::var_os("CARGO_FEATURE_NXP_HOST").is_some() {
        compile_nxp_host(&manifest);
    }
}
