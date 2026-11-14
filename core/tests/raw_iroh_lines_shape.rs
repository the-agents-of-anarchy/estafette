#[cfg(test)]
#[path = "common/remove_scratch.rs"]
mod remove_scratch;

#[cfg(test)]
#[path = "support/raw_iroh_lines_trace.rs"]
mod raw_iroh_lines_trace;

#[cfg(test)]
#[path = "support/raw_iroh_lines_header.rs"]
mod raw_iroh_lines_header;

/// The shape of a raw line of `spec/observability.md` decision 7, through the install the composition root calls, in a process of its own.
#[cfg(test)]
mod raw_iroh_lines_shape {
    use std::fs;
    use std::sync::Arc;

    use estafette::trace::iroh_lines::install;
    use estafette::trace::{TraceBounds, TraceEffects, TraceStop};
    use iroh::SecretKey;

    use super::raw_iroh_lines_header::{header, raw_file_opened_line};
    use super::raw_iroh_lines_trace::{frozen_clocks, scratch_trace_directory, started};
    use super::remove_scratch::remove_scratch;

    const ENDPOINT_SEED: u8 = 3;
    const SPEC_NODE_HEX_CHARS: usize = 10;
    const SPEC_IROH_LINE: &str = "iroh_line";

    /// How iroh's `Side` reads in its lines.
    #[derive(Debug)]
    enum Side {
        Client,
    }

    #[test]
    fn observability_decision_7_a_raw_line_holds_the_header_then_the_target_the_level_the_message_and_the_fields_as_iroh_words_them()
     {
        let (root, directory) = scratch_trace_directory("shape");
        let trace = started(
            &directory,
            &TraceEffects::of_the_operating_system(frozen_clocks()),
            TraceBounds::SPEC,
        );
        let id = SecretKey::from_bytes(&[ENDPOINT_SEED; 32]).public();
        let node = data_encoding::HEXLOWER.encode(id.as_bytes());
        let node = node.get(..SPEC_NODE_HEX_CHARS).unwrap();

        install(Arc::clone(&trace)).unwrap();
        tracing::debug!(
            target: "iroh::_events::conn::connected",
            conn_id = 7_u64,
            side = ?Side::Client,
            remote_id = %"ab12c",
            alpn = "estafette/direct/1"
        );
        trace.note_endpoint(id);
        tracing::trace!(
            target: "iroh::socket::remote_map::remote_state",
            path_id = 3_u64,
            "sending datagram to {}",
            "the selected path"
        );
        assert!(matches!(
            trace.finish(),
            [TraceStop::Drained, TraceStop::Drained]
        ));

        assert_eq!(
            fs::read_to_string(&trace.files().raw).unwrap(),
            format!(
                "{}\
                 {},\"target\":\"iroh::_events::conn::connected\",\"level\":\"DEBUG\",\"fields\":{{\"alpn\":\"estafette/direct/1\",\"conn_id\":\"7\",\"remote_id\":\"ab12c\",\"side\":\"Client\"}}}}\n\
                 {},\"target\":\"iroh::socket::remote_map::remote_state\",\"level\":\"TRACE\",\"message\":\"sending datagram to the selected path\",\"fields\":{{\"path_id\":\"3\"}}}}\n",
                raw_file_opened_line(),
                header(1, None, SPEC_IROH_LINE),
                header(2, Some(node), SPEC_IROH_LINE),
            )
        );
        assert_eq!(fs::read_to_string(&trace.files().events).unwrap(), "");
        remove_scratch(&root).unwrap();
    }
}
