use dicta_core::{AppSnapshot, NewNote, NoteStatus, ProviderMode, store::Store};

#[test]
fn publishing_level_updates_shares_unchanged_history_and_preserves_json() {
    let store = Store::in_memory().unwrap();
    store
        .record_note(NewNote {
            created_at_ms: 1,
            original: "texto original de prueba".repeat(1000),
            text: "texto final de prueba".repeat(1000),
            captured_ms: 1000,
            sent_ms: 1000,
            provider: ProviderMode::Subscription,
            status: NoteStatus::Completed,
            error: None,
            audio_filename: None,
            audio_bytes: 0,
            input_tokens: None,
            output_tokens: None,
        })
        .unwrap();
    let mut current = AppSnapshot::new(Default::default());
    current.notes = store.notes("", 100).unwrap().into();
    let published = current.clone();
    current.level = 0.5;
    current.level_seq += 1;
    assert_eq!(
        current.notes.as_ptr(),
        published.notes.as_ptr(),
        "publishing a level must not duplicate note text"
    );
    store
        .edit_note_text(current.notes[0].id, "edición nueva")
        .unwrap();
    current.notes = store.notes("", 100).unwrap().into();
    assert_ne!(current.notes[0].text, published.notes[0].text);
    let json = serde_json::to_value(&current).unwrap();
    assert!(json["notes"].is_array());
    let decoded: AppSnapshot = serde_json::from_value(json).unwrap();
    assert_eq!(decoded.notes[0].text, "edición nueva");
}
