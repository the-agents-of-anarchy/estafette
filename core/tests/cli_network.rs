#[cfg(test)]
mod network_commands {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::{Command, Output};

    use data_encoding::HEXLOWER;
    use estafette::config::{HostOrigin, NetworkConfig};
    use estafette::crypto::verify_host_certificate;
    use estafette::invite::{parse_invitation, render_host_line};
    use estafette::keys::{parse_public_key, parse_secret_key, render_public_key};
    use estafette::wire::{HostAddr, RelayUrl, Version};
    use iroh::{PublicKey, SecretKey, Signature};

    const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
    const OWNER_ONLY_FILE_MODE: u32 = 0o600;
    const PERMISSION_BITS_MASK: u32 = 0o777;
    const NETWORK: &str = "home-lab";
    const SECRET_KEY_FILE_LENGTH: usize = 65;
    const CERTIFICATE_TEXT_LENGTH: usize = 129;

    struct Machine {
        root: PathBuf,
    }

    impl Machine {
        fn fresh(test_name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "estafette-cli-network-{test_name}-{}",
                std::process::id()
            ));
            if root.exists() {
                fs::remove_dir_all(&root).unwrap();
            }
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn run(&self, arguments: &[&str]) -> Output {
            Command::new(BINARY)
                .args(arguments)
                .env_clear()
                .env("HOME", &self.root)
                .env("XDG_CONFIG_HOME", self.root.join("config"))
                .env("XDG_DATA_HOME", self.root.join("data"))
                .env("XDG_RUNTIME_DIR", self.root.join("runtime"))
                .output()
                .unwrap()
        }

        fn networks_directory(&self) -> PathBuf {
            self.root.join("config/estafette/networks")
        }

        fn config_file(&self, network: &str) -> PathBuf {
            self.networks_directory().join(format!("{network}.toml"))
        }

        fn owner_key_file(&self, network: &str) -> PathBuf {
            self.networks_directory()
                .join(format!("{network}.owner.key"))
        }
    }

    fn mode_of(path: &PathBuf) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & PERMISSION_BITS_MASK
    }

    fn host_address(seed: u8, relay: &str) -> HostAddr {
        HostAddr {
            v: Version::CURRENT,
            id: *SecretKey::from_bytes(&[seed; 32]).public().as_bytes(),
            relay: RelayUrl::try_from(relay.to_owned()).unwrap(),
        }
    }

    fn host_line(host: &HostAddr) -> String {
        render_host_line(host).unwrap()
    }

    fn stdout_of(output: &Output) -> String {
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        String::from_utf8(output.stdout.clone()).unwrap()
    }

    #[test]
    fn spec_3_13_create_network_writes_owner_only_config_and_owner_key_that_agree() {
        let machine = Machine::fresh("create");

        let output = machine.run(&["network", "create", NETWORK]);

        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let config_file = machine.config_file(NETWORK);
        let owner_key_file = machine.owner_key_file(NETWORK);
        assert_eq!(mode_of(&config_file), OWNER_ONLY_FILE_MODE);
        assert_eq!(mode_of(&owner_key_file), OWNER_ONLY_FILE_MODE);
        let owner_key_text = fs::read_to_string(&owner_key_file).unwrap();
        assert_eq!(owner_key_text.len(), SECRET_KEY_FILE_LENGTH);
        let owner_public_key = parse_secret_key(&owner_key_text).unwrap().public();
        let config = NetworkConfig::load(&config_file).unwrap();
        assert_eq!(config.owner, render_public_key(&owner_public_key));
        assert_eq!(config.hosts.len(), 0);
    }

    #[test]
    fn spec_3_13_create_network_refuses_an_existing_network_and_keeps_its_owner_key() {
        let machine = Machine::fresh("create-twice");
        machine.run(&["network", "create", NETWORK]);
        let owner_key_before = fs::read_to_string(machine.owner_key_file(NETWORK)).unwrap();

        let output = machine.run(&["network", "create", NETWORK]);

        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(output.stdout.len(), 0);
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(
            message.starts_with("network home-lab already has"),
            "{message}"
        );
        assert_eq!(message.lines().count(), 1);
        assert_eq!(
            fs::read_to_string(machine.owner_key_file(NETWORK)).unwrap(),
            owner_key_before
        );
    }

    #[test]
    fn spec_3_13_added_hosts_are_configured_and_the_invitation_names_every_one() {
        let machine = Machine::fresh("add-host-invitation");
        machine.run(&["network", "create", NETWORK]);
        let first = host_address(7, "https://relay-one.example/");
        let second = host_address(9, "https://relay-two.example.net/");

        let first_output = machine.run(&["network", "add-host", NETWORK, &host_line(&first)]);
        let second_output = machine.run(&["network", "add-host", NETWORK, &host_line(&second)]);
        let invitation_output = machine.run(&["network", "show-invitation", NETWORK]);

        assert_eq!(stdout_of(&first_output).len(), 0);
        assert_eq!(stdout_of(&second_output).len(), 0);
        let config = NetworkConfig::load(&machine.config_file(NETWORK)).unwrap();
        assert_eq!(config.hosts.len(), 2);
        assert!(
            config
                .hosts
                .iter()
                .all(|host| host.origin == HostOrigin::Configured && host.ts.is_none())
        );
        let invitation_text = stdout_of(&invitation_output);
        let invitation = parse_invitation(invitation_text.trim_end()).unwrap();
        assert!(invitation_text.ends_with('\n'));
        assert_eq!(invitation.network, NETWORK);
        assert_eq!(HEXLOWER.encode(&invitation.secret), config.secret);
        assert_eq!(HEXLOWER.encode(&invitation.owner), config.owner);
        let mut expected_hosts = vec![first, second];
        expected_hosts.sort_by_key(|host| HEXLOWER.encode(&host.id));
        assert_eq!(invitation.hosts, expected_hosts);
    }

    #[test]
    fn spec_3_13_certify_host_without_the_owner_key_file_fails_with_exit_1() {
        let machine = Machine::fresh("certify-without-key");
        let host = host_address(7, "https://relay-one.example/");

        let output = machine.run(&["network", "certify", NETWORK, &host_line(&host)]);

        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(output.stdout.len(), 0);
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(
            message.starts_with("network home-lab has no owner key file here"),
            "{message}"
        );
        assert_eq!(message.lines().count(), 1);
    }

    #[test]
    fn spec_3_4_certify_host_prints_the_owner_signature_over_the_host_node_id() {
        let machine = Machine::fresh("certify");
        machine.run(&["network", "create", NETWORK]);
        let host = host_address(7, "https://relay-one.example/");

        let output = machine.run(&["network", "certify", NETWORK, &host_line(&host)]);

        let certificate_text = stdout_of(&output);
        assert_eq!(certificate_text.len(), CERTIFICATE_TEXT_LENGTH);
        assert!(certificate_text.ends_with('\n'));
        let certificate_hex = certificate_text.trim_end();
        assert_eq!(certificate_hex, certificate_hex.to_ascii_lowercase());
        let certificate_bytes: [u8; 64] = HEXLOWER
            .decode(certificate_hex.as_bytes())
            .unwrap()
            .try_into()
            .unwrap();
        let config = NetworkConfig::load(&machine.config_file(NETWORK)).unwrap();
        let owner = parse_public_key(&config.owner).unwrap();
        let host_id = PublicKey::from_bytes(&host.id).unwrap();
        assert_eq!(
            verify_host_certificate(&owner, &host_id, &Signature::from_bytes(&certificate_bytes)),
            Ok(())
        );
    }

    #[test]
    fn spec_3_13_joining_an_invitation_reproduces_the_network_without_the_owner_key() {
        let owner_machine = Machine::fresh("join-owner");
        let member_machine = Machine::fresh("join-member");
        owner_machine.run(&["network", "create", NETWORK]);
        let host = host_address(7, "https://relay-one.example/");
        owner_machine.run(&["network", "add-host", NETWORK, &host_line(&host)]);
        let invitation_text =
            stdout_of(&owner_machine.run(&["network", "show-invitation", NETWORK]));

        let join_output = member_machine.run(&["network", "join", invitation_text.trim_end()]);

        assert_eq!(stdout_of(&join_output).len(), 0);
        assert_eq!(
            mode_of(&member_machine.config_file(NETWORK)),
            OWNER_ONLY_FILE_MODE
        );
        assert!(!member_machine.owner_key_file(NETWORK).exists());
        let member_invitation =
            stdout_of(&member_machine.run(&["network", "show-invitation", NETWORK]));
        assert_eq!(member_invitation, invitation_text);
        let owner_config = NetworkConfig::load(&owner_machine.config_file(NETWORK)).unwrap();
        let member_config = NetworkConfig::load(&member_machine.config_file(NETWORK)).unwrap();
        assert_eq!(member_config, owner_config);
    }

    #[test]
    fn spec_3_13_joining_an_invitation_refuses_a_network_already_configured_here() {
        let machine = Machine::fresh("join-twice");
        machine.run(&["network", "create", NETWORK]);
        let host = host_address(7, "https://relay-one.example/");
        machine.run(&["network", "add-host", NETWORK, &host_line(&host)]);
        let invitation_text = stdout_of(&machine.run(&["network", "show-invitation", NETWORK]));
        let config_before = fs::read_to_string(machine.config_file(NETWORK)).unwrap();

        let output = machine.run(&["network", "join", invitation_text.trim_end()]);

        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(output.stdout.len(), 0);
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .starts_with("network home-lab already has")
        );
        assert_eq!(
            fs::read_to_string(machine.config_file(NETWORK)).unwrap(),
            config_before
        );
    }

    #[test]
    fn spec_3_13_show_invitation_refuses_a_network_that_names_no_host() {
        let machine = Machine::fresh("invitation-without-host");
        machine.run(&["network", "create", NETWORK]);

        let output = machine.run(&["network", "show-invitation", NETWORK]);

        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(output.stdout.len(), 0);
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(
            message.starts_with("network home-lab names no host yet"),
            "{message}"
        );
    }

    #[test]
    fn spec_3_13_commands_on_a_network_not_configured_here_fail_with_exit_1() {
        let machine = Machine::fresh("not-configured");
        let host = host_address(7, "https://relay-one.example/");

        let invitation = machine.run(&["network", "show-invitation", NETWORK]);
        let add_host = machine.run(&["network", "add-host", NETWORK, &host_line(&host)]);

        for output in [invitation, add_host] {
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert_eq!(output.stdout.len(), 0);
            assert!(
                String::from_utf8(output.stderr)
                    .unwrap()
                    .starts_with("network home-lab has no configuration file here")
            );
        }
    }

    #[test]
    fn spec_3_13_create_network_records_on_stderr_that_no_invitation_exists_yet() {
        let machine = Machine::fresh("create-record");

        let output = machine.run(&["network", "create", NETWORK]);

        assert_eq!(stdout_of(&output).len(), 0);
        let record = String::from_utf8(output.stderr).unwrap();
        assert!(
            record.starts_with("created network home-lab: it names no host yet"),
            "{record}"
        );
    }

    #[test]
    fn spec_3_13_a_malformed_command_line_is_a_usage_error_with_exit_2() {
        let machine = Machine::fresh("usage");
        let host = host_address(7, "https://relay-one.example/");
        let malformed_command_lines: [Vec<&str>; 5] = [
            vec!["network", "create", "Bad_Name"],
            vec!["network", "create"],
            vec!["network", "join", "estafette-invite-1:zz"],
            vec!["network", "certify", NETWORK, "estafette-invite-1:00"],
            vec!["network", "frobnicate"],
        ];

        for arguments in &malformed_command_lines {
            let output = machine.run(arguments);
            assert_eq!(output.status.code(), Some(2), "{arguments:?}: {output:?}");
            assert_eq!(output.stdout.len(), 0);
        }
        let valid_line_with_bad_network =
            machine.run(&["network", "add-host", "9lives", &host_line(&host)]);
        assert_eq!(valid_line_with_bad_network.status.code(), Some(2));
    }

    #[test]
    fn spec_3_13_help_states_what_each_command_reads_produces_and_refuses() {
        let machine = Machine::fresh("help");

        for subcommand in ["create", "join", "add-host", "show-invitation", "certify"] {
            let output = machine.run(&["network", subcommand, "--help"]);
            let help = stdout_of(&output);
            for stated in ["Reads:", "Produces:", "Refuses:"] {
                assert!(help.contains(stated), "{subcommand}: {help}");
            }
        }
    }

    #[test]
    fn spec_3_13_create_network_refuses_a_name_already_joined_here_and_writes_no_owner_key() {
        let owner_machine = Machine::fresh("create-over-join-owner");
        let member_machine = Machine::fresh("create-over-join-member");
        owner_machine.run(&["network", "create", NETWORK]);
        let host = host_address(7, "https://relay-one.example/");
        owner_machine.run(&["network", "add-host", NETWORK, &host_line(&host)]);
        let invitation_text =
            stdout_of(&owner_machine.run(&["network", "show-invitation", NETWORK]));
        member_machine.run(&["network", "join", invitation_text.trim_end()]);

        let output = member_machine.run(&["network", "create", NETWORK]);

        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(!member_machine.owner_key_file(NETWORK).exists());
    }

    #[test]
    fn spec_3_13_create_network_refuses_a_name_whose_owner_key_remains_and_keeps_that_key() {
        let machine = Machine::fresh("create-over-owner-key");
        machine.run(&["network", "create", NETWORK]);
        let owner_key_before = fs::read_to_string(machine.owner_key_file(NETWORK)).unwrap();
        fs::remove_file(machine.config_file(NETWORK)).unwrap();

        let output = machine.run(&["network", "create", NETWORK]);

        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(!machine.config_file(NETWORK).exists());
        assert_eq!(
            fs::read_to_string(machine.owner_key_file(NETWORK)).unwrap(),
            owner_key_before
        );
    }
}
