package policy

import rego.v1

# This policy validates multiple TEE platforms
# The policy is meant to capture the TCB requirements
# for confidential instance.

# This policy is used to generate an EAR Appraisal.
# Specifically it generates an AR4SI result.
# More information on AR4SI can be found at
# <https://datatracker.ietf.org/doc/draft-ietf-rats-ar4si/>

# For the 'executables' trust claim, the value 33 stands for
# "Runtime memory includes executables, scripts, files, and/or
#  objects which are not recognized."
default executables := 33

# For the 'hardware' trust claim, the value 97 stands for
# "A Verifier does not recognize an Attester's hardware or
#  firmware, but it should be recognized."
default hardware := 97

# For the 'configuration' trust claim the value 36 stands for
# "Elements of the configuration relevant to security are
#  unavailable to the Verifier."
default configuration := 36

# For the 'filesystem' trust claim, the value 35 stands for
# "File system integrity cannot be verified or is compromised."
default file_system := 35

### The following functions are for parsing UEFI event logs
### These functions are chosen when the related verifier is using 'deps/eventlog'
### crate

# Parse grub algorithm and digest
parse_grub(uefi_event_logs) := grub if {
        some i, j
        uefi_event_logs[i].type_name == "EV_EFI_BOOT_SERVICES_APPLICATION"
        contains(uefi_event_logs[i].details.device_paths[j], "grub")
        grub := {
                "alg": uefi_event_logs[i].digests[0].alg,
                "value": uefi_event_logs[i].digests[0].digest,
        }
}

# Parse shim algorithm and digest
parse_shim(uefi_event_logs) := shim if {
        some i, j
        uefi_event_logs[i].type_name == "EV_EFI_BOOT_SERVICES_APPLICATION"
        contains(uefi_event_logs[i].details.device_paths[j], "shim")
        shim := {
                "alg": uefi_event_logs[i].digests[0].alg,
                "value": uefi_event_logs[i].digests[0].digest,
        }
}

# Parse kernel algorithm and digest
parse_kernel(uefi_event_logs) := kernel if {
        some i
        uefi_event_logs[i].type_name == "EV_IPL"
        contains(uefi_event_logs[i].details.string, "Kernel")
        kernel := {
                "alg": uefi_event_logs[i].digests[0].alg,
                "value": uefi_event_logs[i].digests[0].digest,
        }
}

# Parse initrd algorithm and digest
parse_initrd(uefi_event_logs) := initrd if {
        some i
        uefi_event_logs[i].type_name == "EV_IPL"
        contains(uefi_event_logs[i].details.string, "Initrd")
        initrd := {
                "alg": uefi_event_logs[i].digests[0].alg,
                "value": uefi_event_logs[i].digests[0].digest,
        }
}

# Validate GRUB boot measurements (grub, shim, kernel, initrd)
validate_boot_measurements_grub(uefi_event_logs) if {
        grub := parse_grub(uefi_event_logs)
        shim := parse_shim(uefi_event_logs)
        initrd := parse_initrd(uefi_event_logs)
        kernel := parse_kernel(uefi_event_logs)
        components := [
                {"name": "grub", "value": grub.value, "alg": grub.alg},
                {"name": "shim", "value": shim.value, "alg": shim.alg},
                {"name": "initrd", "value": initrd.value, "alg": initrd.alg},
                {"name": "kernel", "value": kernel.value, "alg": kernel.alg},
        ]
        every component in components {
                measurement_key := sprintf("measurement.%s.%s", [component.name, component.alg])
                component.value in data.reference[measurement_key]
        }
}

# Parse UKI (Unified Kernel Image) algorithm and digest
parse_uki(uefi_event_logs) := uki if {
        some i, j
        uefi_event_logs[i].type_name == "EV_EFI_BOOT_SERVICES_APPLICATION"
        contains(uefi_event_logs[i].details.device_paths[j], "File(\\EFI\\BOOT\\BOOTX64.EFI)")
        uki := {
                "alg": uefi_event_logs[i].digests[0].alg,
                "value": uefi_event_logs[i].digests[0].digest,
        }
}

# Validate UKI boot measurements
validate_boot_measurements_uki(uefi_event_logs) if {
        uki := parse_uki(uefi_event_logs)
        measurement_key := sprintf("measurement.uki.%s", [uki.alg])
        uki.value in data.reference[measurement_key]
}

# Generic function to validate kernel cmdline for any platform and algorithm
validate_kernel_cmdline_uefi(uefi_event_logs) if {
        some prefix in ["grub_cmd linux", "kernel_cmdline", "grub_kernel_cmdline"]
        some i
        uefi_event_logs[i].type_name == "EV_IPL"
        startswith(uefi_event_logs[i].details.string, prefix)
        measurement_key := sprintf("measurement.kernel_cmdline.%s", [uefi_event_logs[i].digests[0].alg])
        uefi_event_logs[i].digests[0].digest in data.reference[measurement_key]
}

# Function to check the file measurements from Measurement_tool integrity
validate_aael_file_measurements(uefi_event_logs) if {
        aael := [e |
                e := uefi_event_logs[_]
                e.type_name == "EV_EVENT_TAG"
                e.details.unicode_name == "AAEL"
                e.details.data.domain == "file"
        ]
        every e in aael {
                key := sprintf("measurement.%s.%s", [e.details.data.domain, e.details.data.operation])
                e.details.data.content in data.reference[key]
        }
}

##### TDX

# Validate executables for GRUB boot mode
executables := 3 if {
        # Check the kernel, initrd, shim and grub measurements for any supported algorithm
        validate_boot_measurements_grub(input.tdx.uefi_event_logs)

}

# Validate executables for UKI boot mode
executables := 3 if {
        # Check the UKI image measurement for any supported algorithm
        validate_boot_measurements_uki(input.tdx.uefi_event_logs)

}

hardware := 2 if {
        # Check the quote is a TDX quote signed by Intel SGX Quoting Enclave
        input.tdx.quote.header.tee_type == "81000000"
        input.tdx.quote.header.vendor_id == "939a7233f79c4ca9940a0db3957f0607"
        # Check TDX Module version and its hash. Also check OVMF code hash.
        # input.tdx.quote.body.mr_seam in data.reference["tdx.mr_seam"]
        # input.tdx.quote.body.tcb_svn in data.reference["tdx.tcb_svn"]
        # input.tdx.quote.body.mr_td in data.reference["tdx.mr_td"]
}

# For GRUB mode: validate boot measurements and kernel cmdline
configuration := 2 if {
        validate_boot_measurements_grub(input.tdx.uefi_event_logs)
        validate_kernel_cmdline_uefi(input.tdx.uefi_event_logs)
}

# For UKI mode: skip kernel cmdline check (embedded in UKI image)
configuration := 2 if {
        validate_boot_measurements_uki(input.tdx.uefi_event_logs)
}

file_system := 2 if {
        input.tdx

        # Root file system hash is included in initrd binary, so we don't need to check it separately
}
