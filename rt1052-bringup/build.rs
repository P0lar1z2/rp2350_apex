use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)]
mod macro_generator {
    include!("../build.rs");

    pub fn generate(out: &std::path::Path) {
        compile_macro_config(out);
    }
}

fn run(mut command: Command, description: &str) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("failed to run {description}: {error}"));
    assert!(status.success(), "{description} failed with {status}");
}

fn stage_rt1052_ehci(source: &Path, out: &Path) -> PathBuf {
    let mut contents = fs::read_to_string(source)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source.display()));
    let repeated_interrupt_charge = r#"                else /* iso bandwidth is allocated three times */
                {
                    frameBandwidths[ehciPipePointer->startUframe + 1U] += ehciPipePointer->dataTime;
                    frameBandwidths[ehciPipePointer->startUframe + 2U] += ehciPipePointer->dataTime;
                    frameBandwidths[ehciPipePointer->startUframe + 3U] += ehciPipePointer->dataTime;
                }"#;
    let single_interrupt_charge = r#"                else /* interrupt payload occupies the TT once; complete-split retries are accounted separately */
                {
                    frameBandwidths[ehciPipePointer->startUframe + 1U] += ehciPipePointer->dataTime;
                }"#;
    assert_eq!(
        contents.matches(repeated_interrupt_charge).count(),
        1,
        "NXP EHCI interrupt bandwidth accounting changed upstream"
    );
    contents = contents.replacen(repeated_interrupt_charge, single_interrupt_charge, 1);

    let repeated_candidate_charge = r#"                    index = (uint8_t)(uframeIntervalIndex + 1U);
                    for (; index <= (uframeIntervalIndex + 3U); ++index) /* data bandwidth number is 3.
                                                                             uframeIntervalIndex don't exceed 4, so
                                                                             index cannot exceed 7 */
                    {
                        if (frameTimes[index] + timeData > s_SlotMaxBandwidth[index])
                        {
                            allocateOk = 0;
                            break;
                        }
                    }"#;
    let single_candidate_charge = r#"                    index = (uint8_t)(uframeIntervalIndex + 1U);
                    frameTimes[index] += (uint16_t)timeData;
                    for (; index < 7U; ++index)
                    {
                        if (frameTimes[index] > s_SlotMaxBandwidth[index])
                        {
                            frameTimes[index + 1U] +=
                                (uint16_t)(frameTimes[index] - s_SlotMaxBandwidth[index]);
                            frameTimes[index] = s_SlotMaxBandwidth[index];
                        }
                        else
                        {
                            break;
                        }
                    }
                    if (frameTimes[index] > s_SlotMaxBandwidth[index])
                    {
                        allocateOk = 0U;
                    }"#;
    assert_eq!(
        contents.matches(repeated_candidate_charge).count(),
        1,
        "NXP EHCI interrupt candidate accounting changed upstream"
    );
    contents = contents.replacen(repeated_candidate_charge, single_candidate_charge, 1);

    let staged = out.join("usb_host_ehci_rt1052.c");
    fs::write(&staged, contents)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", staged.display()));
    staged
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
    let ehci_source = usb.join("host/usb_host_ehci.c");
    let staged_ehci = stage_rt1052_ehci(&ehci_source, &out);
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
        staged_ehci,
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
    for source in [
        "csrc/nxp_host_ffi.c",
        "csrc/nxp_host_ffi.h",
        "csrc/osa_baremetal.c",
        "csrc/usb_host_config.h",
    ] {
        println!("cargo:rerun-if-changed={}", manifest.join(source).display());
    }
    println!("cargo:rerun-if-changed={}", ehci_source.display());
}

fn compile_nxp_enet(manifest: &Path) {
    let vendor = manifest
        .parent()
        .expect("bring-up crate must be inside the repository")
        .join(".vendor");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let device = vendor.join("mcux-devices-rt/RT1050/MIMXRT1052");
    let core = vendor.join("mcuxsdk-core");
    let cmsis = vendor.join("mcu-sdk-cmsis");
    let includes = [
        manifest.join("csrc"),
        device.clone(),
        vendor.join("mcux-devices-rt/RT1050/periph"),
        device.join("drivers"),
        core.join("drivers/common"),
        core.join("drivers/enet"),
        core.join("drivers/cache/armv7-m7"),
        cmsis.join("Core/Include"),
    ];
    let sources = [
        manifest.join("csrc/nxp_enet_ffi.c"),
        core.join("drivers/common/fsl_common.c"),
        device.join("drivers/fsl_clock.c"),
        device.join("system_MIMXRT1052.c"),
        core.join("drivers/enet/fsl_enet.c"),
    ];
    for path in includes.iter().chain(sources.iter()) {
        assert!(path.exists(), "missing NXP SDK path: {}", path.display());
    }
    let mut objects = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let stem = source.file_stem().expect("C source stem").to_string_lossy();
        let object = out.join(format!("enet_{index:02}_{stem}.o"));
        let mut command = Command::new("arm-none-eabi-gcc");
        command.args([
            "-c", "-mcpu=cortex-m7", "-mthumb", "-mfpu=fpv5-d16", "-mfloat-abi=hard",
            "-std=gnu11", "-Os", "-ffunction-sections", "-fdata-sections", "-fno-builtin",
            "-Wall", "-Werror", "-DCPU_MIMXRT1052CVL5B", "-DSDK_DEBUGCONSOLE=2",
            "-DDATA_SECTION_IS_CACHEABLE=0", "-DNDEBUG",
        ]);
        for include in &includes {
            command.arg("-I").arg(include);
        }
        command.arg(source).arg("-o").arg(&object);
        run(command, &format!("compile {}", source.display()));
        objects.push(object);
    }
    let archive = out.join("libnxp_enet.a");
    let mut command = Command::new("arm-none-eabi-ar");
    command.arg("crs").arg(&archive).args(&objects);
    run(command, "archive NXP ENET objects");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=nxp_enet");
    println!("cargo:rerun-if-changed={}", manifest.join("csrc/nxp_enet_ffi.c").display());
    println!("cargo:rerun-if-changed={}", manifest.join("csrc/nxp_enet_ffi.h").display());
}

fn main() {
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"));
    println!("cargo:rustc-link-search={}", env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rerun-if-changed=memory.x");

    if env::var_os("CARGO_FEATURE_FLASH_XIP").is_some() {
        println!("cargo:rustc-link-arg=--defsym=__flash_xip=1");
    }

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let previous_dir = env::current_dir().expect("build script current directory");
    let repository = manifest
        .parent()
        .expect("bring-up crate must be inside the repository");
    env::set_current_dir(repository).expect("enter repository for macro config generation");
    macro_generator::generate(&out);
    env::set_current_dir(previous_dir).expect("restore build script current directory");
    println!("cargo:rerun-if-changed=../macro_config.toml");
    println!("cargo:rerun-if-changed=../build.rs");

    if env::var_os("CARGO_FEATURE_NXP_HOST").is_some() {
        compile_nxp_host(&manifest);
    }
    if env::var_os("CARGO_FEATURE_NXP_ENET").is_some() {
        compile_nxp_enet(&manifest);
    }
}
