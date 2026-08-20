use super::*;
use atlas_rpc::InProcessTransport;

#[tokio::test]
async fn queue_mutations_invalidate_list_cursors() {
    let state = BridgeState::new(AcpQueueStore::default());
    let thread_id = ThreadId("thread".into());
    state.upsert_summary(ThreadSummary {
        id: thread_id.clone(),
        backend: BackendId("test".into()),
        cwd: "/workspace".into(),
        additional_directories: Vec::new(),
        title: None,
        updated_at: None,
        status: ThreadStatus::Active,
    });
    state.queues.queues.lock().unwrap().insert(
        thread_id.0.clone(),
        StoredQueue {
            submissions: ["one", "two"]
                .into_iter()
                .map(|id| QueuedSubmission {
                    id: QueuedSubmissionId(id.into()),
                    input: vec![ContentBlock::Text { text: id.into() }],
                    client_user_message_id: format!("message-{id}"),
                })
                .collect(),
            paused: false,
            revision: 7,
        },
    );
    let (transport, _other) = InProcessTransport::pair();
    let bridge = AcpBridge {
        agent: v2::AgentHandle::new(Peer::new(transport)),
        backend: BackendId("test".into()),
        state,
        supports_close: false,
    };

    let archive_error = bridge
        .thread_archive(ThreadArchiveParams {
            thread_id: thread_id.clone(),
        })
        .await
        .unwrap_err();
    let delete_error = bridge
        .thread_delete(ThreadDeleteParams {
            thread_id: thread_id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(archive_error.code.as_deref(), Some("unsupported_operation"));
    assert_eq!(delete_error.code.as_deref(), Some("unsupported_operation"));

    let first_page = bridge
        .thread_queue_list(ThreadQueueListParams {
            thread_id: thread_id.clone(),
            cursor: None,
            limit: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(first_page.data[0].id, QueuedSubmissionId("one".into()));
    let cursor = first_page.next_cursor.unwrap();

    bridge
        .thread_queue_update(ThreadQueueUpdateParams {
            thread_id: thread_id.clone(),
            queued_submission_id: QueuedSubmissionId("one".into()),
            input: vec![ContentBlock::Text {
                text: "updated".into(),
            }],
        })
        .await
        .unwrap();
    let error = bridge
        .thread_queue_list(ThreadQueueListParams {
            thread_id,
            cursor: Some(cursor),
            limit: Some(1),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code.as_deref(), Some("stale_queue_cursor"));
}

#[tokio::test]
async fn reorder_requires_an_exact_permutation() {
    let state = BridgeState::new(AcpQueueStore::default());
    let thread_id = ThreadId("thread".into());
    state.upsert_summary(ThreadSummary {
        id: thread_id.clone(),
        backend: BackendId("test".into()),
        cwd: "/workspace".into(),
        additional_directories: Vec::new(),
        title: None,
        updated_at: None,
        status: ThreadStatus::Active,
    });
    state.queues.queues.lock().unwrap().insert(
        thread_id.0.clone(),
        StoredQueue {
            submissions: ["one", "two"]
                .into_iter()
                .map(|id| QueuedSubmission {
                    id: QueuedSubmissionId(id.into()),
                    input: Vec::new(),
                    client_user_message_id: id.into(),
                })
                .collect(),
            paused: false,
            revision: 0,
        },
    );
    let (transport, _other) = InProcessTransport::pair();
    let bridge = AcpBridge {
        agent: v2::AgentHandle::new(Peer::new(transport)),
        backend: BackendId("test".into()),
        state,
        supports_close: false,
    };

    let error = bridge
        .thread_queue_reorder(ThreadQueueReorderParams {
            thread_id,
            queued_submission_ids: vec![QueuedSubmissionId("one".into())],
        })
        .await
        .unwrap_err();
    assert_eq!(error.code.as_deref(), Some("invalid_queue_order"));
}
