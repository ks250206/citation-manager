//! #20 "Save Current PDF…": the export command provider is registered through the #16
//! registry (palette, keymap, dispatch availability) and the request/status logic never
//! reports an incomplete or failed export as success. Native dialog/toolbar wiring is in
//! `native::pdf_export_ui` and the Xvfb checklist.
use cm_desktop::{
    commands::{
        Availability, CommandContext, CommandRegistry, DisabledReason, Requirement,
        deferred_commands, pdf_export_commands, pdf_export_ids,
    },
    keymap::{Context, InputState, KeyResolver, KeyStroke, Keymap, Mode, Resolution},
    palette::rank,
    pdf_export::{ExportState, suggested_name},
};
use cm_protocol::{
    annotations::{AnnotationId, AnnotationKind, COLORS},
    attachments::{Omitted, PdfExportFailure, PdfExportReport, PdfReply, PdfRequest},
    domain::AttachmentId,
    ipc::{Capability, Operation, PdfCommand},
    library::ServiceError,
};
use serde_json::json;
use std::path::PathBuf;

const SAVE: &str = pdf_export_ids::SAVE;

fn context(handlers: &'static [&'static str], has_selection: bool) -> CommandContext<'static> {
    CommandContext {
        connected_handlers: handlers,
        library_ready: true,
        has_selection,
        busy: false,
    }
}

#[test]
fn pdf_save_is_a_connected_provider_in_the_registry_not_a_placeholder() {
    assert_eq!(SAVE, "pdf.save");
    assert!(deferred_commands().iter().all(|c| c.id != SAVE));
    let provided = pdf_export_commands();
    assert_eq!(provided.len(), 1);
    assert!(matches!(provided[0].requirement, Requirement::Selection));
    // Registration goes through the ordinary registry rules: valid ID, no duplicates.
    let mut registry = CommandRegistry::default();
    registry.register(provided[0]).unwrap();
    assert!(registry.register(provided[0]).is_err());

    let registry = CommandRegistry::m1();
    let command = registry.get(SAVE).expect("registered in the M1 registry");
    assert_eq!(command.title, "Save Current PDF…");
    assert_eq!(
        registry.availability(SAVE, &context(&[], true)),
        Availability::Disabled(DisabledReason::NotConnected)
    );
    assert_eq!(
        registry.availability(SAVE, &context(&[SAVE], false)),
        Availability::Disabled(DisabledReason::SelectionRequired)
    );
    assert_eq!(
        registry.availability(SAVE, &context(&[SAVE], true)),
        Availability::Enabled
    );
    let busy = CommandContext {
        busy: true,
        ..context(&[SAVE], true)
    };
    assert_eq!(
        registry.availability(SAVE, &busy),
        Availability::Disabled(DisabledReason::Busy)
    );
}

#[test]
fn pdf_save_resolves_from_the_keymap_and_palette_when_connected() {
    let registry = CommandRegistry::m1();
    let document = serde_json::to_vec(&json!({"version": 1, "bindings": [
        {"context": "library", "mode": "normal", "keys": ["g", "p"], "command": SAVE},
    ]}))
    .unwrap();
    let mut resolver = KeyResolver::new(Keymap::parse(&document, &registry).unwrap());
    let state = InputState {
        context: Context::Library,
        mode: Mode::Normal,
        editing: false,
        composing: false,
    };
    let connected = context(&[SAVE], true);
    let key = |k: &str| KeyStroke::parse(k).unwrap();
    assert_eq!(
        resolver.resolve(&key("g"), state, 0, &registry, &connected),
        Resolution::Pending
    );
    assert_eq!(
        resolver.resolve(&key("p"), state, 10, &registry, &connected),
        Resolution::Command {
            id: SAVE.into(),
            availability: Availability::Enabled
        }
    );
    let entries = rank(&registry, &connected, "save pdf", &[]);
    let entry = entries
        .iter()
        .find(|e| e.id == SAVE)
        .expect("palette entry");
    assert_eq!(entry.availability, Availability::Enabled);
}

fn attachment() -> AttachmentId {
    AttachmentId("00000000-0000-7000-8000-000000000001".parse().unwrap())
}
fn omitted() -> Omitted {
    Omitted {
        id: AnnotationId("00000000-0000-7000-8000-000000000002".parse().unwrap()),
        kind: AnnotationKind::Highlight,
        reason: "Not placed on this PDF: copied from a replaced PDF".into(),
    }
}
fn report(omitted: Vec<Omitted>) -> PdfExportReport {
    PdfExportReport {
        path: "/tmp/Paper (annotated).pdf".into(),
        highlights: 3,
        notes: 1,
        preserved: 2,
        omitted,
        original_sha256: "ab".repeat(32),
        max_position_error: 0.0,
        replaced: false,
    }
}

#[test]
fn export_requests_are_ipc_commands_needing_read_and_local_write() {
    let mut state = ExportState::default();
    let destination = PathBuf::from("/home/me/Paper (annotated).pdf");
    let request = state.begin(attachment(), destination.clone(), true, false);
    assert_eq!(state.button_label(Some(attachment())), "Cancel Save");
    let PdfRequest::Export(r) = &request else {
        panic!()
    };
    assert_eq!(
        (
            r.attachment,
            &r.destination,
            r.replace_existing,
            r.allow_omissions
        ),
        (attachment(), &destination, true, false)
    );
    let token = r.cancel.clone();
    let (command, cancel) = PdfCommand::from_request(request);
    assert!(
        cancel.is_some(),
        "cancellation stays client-side like Render"
    );
    assert!(matches!(command, PdfCommand::Export { .. }));
    assert_eq!(command.operation(), Operation::PdfExport);
    assert!(
        Operation::PdfExport
            .requirement()
            .satisfied_by(&[Capability::LibraryRead, Capability::LocalFileWrite].into())
    );
    assert!(
        !Operation::PdfExport
            .requirement()
            .satisfied_by(&[Capability::LibraryRead].into())
    );
    assert!(state.cancel());
    assert!(token.is_cancelled());
    assert!(!state.cancel());
}

#[test]
fn status_never_reports_incomplete_or_failed_exports_as_success() {
    let mut state = ExportState::default();
    let request = || (attachment(), PathBuf::from("/tmp/x.pdf"), false);
    let finish = |state: &mut ExportState, reply| state.finish(request(), Ok(reply));

    let (text, error) = finish(&mut state, PdfReply::Exported(Ok(Box::new(report(vec![])))));
    assert!(
        !error && text.contains("verified") && text.contains("original unchanged"),
        "{text}"
    );
    let (text, error) = finish(
        &mut state,
        PdfReply::Exported(Ok(Box::new(report(vec![omitted()])))),
    );
    assert!(error && text.contains("INCOMPLETE"), "{text}");

    // Unplaced annotations: nothing written, and the next save must be an explicit choice.
    let (text, error) = finish(
        &mut state,
        PdfReply::Exported(Err(PdfExportFailure::Unexportable(vec![omitted()]))),
    );
    assert!(error && text.starts_with("Nothing saved"), "{text}");
    assert_eq!(
        state.button_label(Some(attachment())),
        "Save without 1 unplaced…"
    );
    assert_eq!(state.button_label(None), "Save PDF…");
    let confirm = state.confirm.clone().unwrap();
    assert_eq!(confirm.destination, PathBuf::from("/tmp/x.pdf"));
    let PdfRequest::Export(r) = state.begin(confirm.attachment, confirm.destination, false, true)
    else {
        panic!()
    };
    assert!(r.allow_omissions);
    assert!(state.confirm.is_none());

    for failure in [
        PdfExportFailure::Encrypted,
        PdfExportFailure::Damaged,
        PdfExportFailure::VerificationFailed(vec!["QuadPoints differ".into()]),
        PdfExportFailure::OriginalChanged,
        PdfExportFailure::Destination("A file with this name already exists".into()),
    ] {
        let (text, error) = finish(&mut state, PdfReply::Exported(Err(failure)));
        assert!(error && text.starts_with("PDF not saved"), "{text}");
    }
    let (text, error) = finish(
        &mut state,
        PdfReply::Exported(Err(PdfExportFailure::Cancelled)),
    );
    assert!(!error && text.contains("nothing written"), "{text}");
    let (text, error) = state.finish(request(), Err(ServiceError::Unavailable));
    assert!(error && text.starts_with("PDF not saved"), "{text}");
    assert!(state.running.is_none());
}

#[test]
fn suggested_names_are_single_safe_file_names() {
    assert_eq!(
        suggested_name("Deep Learning"),
        "Deep Learning (annotated).pdf"
    );
    assert_eq!(
        suggested_name("a/b\\c: d?\n論文"),
        "a b c d 論文 (annotated).pdf"
    );
    assert_eq!(suggested_name("  ..  "), "annotated.pdf");
    assert!(suggested_name(&"x".repeat(500)).chars().count() <= 100 + " (annotated).pdf".len());
}

#[test]
fn viewer_and_export_share_the_highlight_palette() {
    for name in COLORS
        .iter()
        .copied()
        .map(Some)
        .chain([None, Some("#123abc")])
    {
        let [r, g, b] = cm_protocol::annotations::rgb(name);
        assert_eq!(
            cm_desktop::annotations::rgb(name),
            u32::from_be_bytes([0, r, g, b]),
            "{name:?}"
        );
    }
}
