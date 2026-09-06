#ifndef UNPEEL_NATIVE_BRIDGE_H
#define UNPEEL_NATIVE_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#define UNPEEL_NATIVE_BRIDGE_OK 1
#define UNPEEL_NATIVE_BRIDGE_HANDLED 1
#define UNPEEL_NATIVE_BRIDGE_UNHANDLED 0
#define UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_INPUT -1
#define UNPEEL_NATIVE_BRIDGE_ERROR_PANIC -2
#define UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION -3
#define UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_HANDLE -4
#define UNPEEL_NATIVE_BRIDGE_ERROR_REMOTE -5

#define UNPEEL_NATIVE_BRIDGE_RELAY_OK 1
#define UNPEEL_NATIVE_BRIDGE_RELAY_GENERATION_CHANGED 2
#define UNPEEL_NATIVE_BRIDGE_RELAY_NOT_SENT 3
#define UNPEEL_NATIVE_BRIDGE_RELAY_OUTCOME_UNKNOWN 4
#define UNPEEL_NATIVE_BRIDGE_RELAY_TIMED_OUT_NOT_SENT 5
#define UNPEEL_NATIVE_BRIDGE_RELAY_TIMED_OUT_OUTCOME_UNKNOWN 6

typedef uint64_t unpeel_native_bridge_remote_handle_t;
typedef uint64_t unpeel_native_bridge_remote_output_page_handle_t;
typedef uint64_t unpeel_native_bridge_platform_adapter_handle_t;

typedef int32_t (*unpeel_native_bridge_relay_request_callback_t)(
    void *context,
    const uint8_t *request_pointer,
    size_t request_length,
    uint64_t required_generation,
    uint64_t timeout_ms,
    uint64_t *out_generation,
    uint8_t **out_pointer,
    size_t *out_length
);
typedef void (*unpeel_native_bridge_relay_bytes_release_callback_t)(
    void *context,
    uint8_t *pointer,
    size_t length
);
typedef void (*unpeel_native_bridge_relay_disconnect_callback_t)(void *context);
typedef void (*unpeel_native_bridge_relay_context_release_callback_t)(void *context);

uint32_t unpeel_native_bridge_abi_version(void);

int32_t unpeel_native_bridge_migrate_legacy_pins(void);

int32_t unpeel_native_bridge_route(
    const uint8_t *request_pointer,
    size_t request_length,
    const uint8_t *context_pointer,
    size_t context_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Register an SSH-backed RemoteSessionBackend. This validates the target but
 * does not start SSH; call remote_bootstrap away from the main thread for the
 * first network request. On success out_handle is non-zero and output is
 * empty. On failure out_handle is zero and output may contain owned UTF-8 JSON.
 */
int32_t unpeel_native_bridge_remote_ssh_open(
    const uint8_t *target_pointer,
    size_t target_length,
    unpeel_native_bridge_remote_handle_t *out_handle,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Register an SSH backend from UTF-8 JSON containing target, launch mode,
 * and optional askpass program + secret. The secret is never returned.
 */
int32_t unpeel_native_bridge_remote_ssh_config_open(
    const uint8_t *config_pointer,
    size_t config_length,
    unpeel_native_bridge_remote_handle_t *out_handle,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Install Unpeel using the same SSH JSON configuration and credentials as
 * remote_ssh_config_open. The remote command is fixed inside Rust. Success
 * returns owned UTF-8 JSON containing the launch mode used.
 */
int32_t unpeel_native_bridge_remote_ssh_install(
    const uint8_t *config_pointer,
    size_t config_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Register the loopback workspace-gateway RemoteSessionBackend from UTF-8
 * JSON containing the absolute unpeel-host program path and the workspace's
 * UNPEEL_HOME. Validation only; the gateway child is spawned by the first
 * remote_bootstrap.
 */
int32_t unpeel_native_bridge_remote_local_gateway_open(
    const uint8_t *config_pointer,
    size_t config_length,
    unpeel_native_bridge_remote_handle_t *out_handle,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Start a reconnecting platform-adapter registration against one workspace's
 * host.sock. Config is UTF-8 JSON containing unpeelHome, instanceID,
 * callbackPort, callbackToken, and capabilities. The callback is loopback-only;
 * the registration and advertised capabilities die with this handle/process.
 */
int32_t unpeel_native_bridge_platform_adapter_start(
    const uint8_t *config_pointer,
    size_t config_length,
    unpeel_native_bridge_platform_adapter_handle_t *out_handle,
    uint8_t **out_pointer,
    size_t *out_length
);

int32_t unpeel_native_bridge_platform_adapter_stop(
    unpeel_native_bridge_platform_adapter_handle_t handle,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Perform one same-user management request against a workspace worker's
 * host.sock. Config is UTF-8 JSON containing unpeelHome and request.
 * Success returns the Host's owned UTF-8 JSON response.
 */
int32_t unpeel_native_bridge_local_host_control(
    const uint8_t *config_pointer,
    size_t config_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Register a paired direct/LAN RemoteSessionBackend without network I/O.
 * endpoint must be the exact trusted-LAN http://.../mobile scope returned by
 * pairing. bearer stays Rust-owned and is never returned or logged. The first
 * network request occurs in remote_bootstrap.
 */
int32_t unpeel_native_bridge_remote_direct_open(
    const uint8_t *endpoint_pointer,
    size_t endpoint_length,
    const uint8_t *bearer_pointer,
    size_t bearer_length,
    unpeel_native_bridge_remote_handle_t *out_handle,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Register a Link-backed RemoteSessionBackend. The callbacks must delegate to
 * the shared Swift RemoteRelayConnection; Rust retains context until close,
 * injects the paired bearer, and owns generation/effect certainty. No network
 * request occurs until remote_bootstrap.
 */
int32_t unpeel_native_bridge_remote_relay_open(
    const uint8_t *bearer_pointer,
    size_t bearer_length,
    void *context,
    unpeel_native_bridge_relay_request_callback_t request_callback,
    unpeel_native_bridge_relay_bytes_release_callback_t bytes_release_callback,
    unpeel_native_bridge_relay_disconnect_callback_t disconnect_callback,
    unpeel_native_bridge_relay_context_release_callback_t context_release_callback,
    unpeel_native_bridge_remote_handle_t *out_handle,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Return the validated typed RemoteBootstrapSnapshot as owned UTF-8 JSON.
 * Failures may return owned JSON with stable `code` and actionable `error`
 * fields. An ordinary failure does not close the handle.
 */
int32_t unpeel_native_bridge_remote_bootstrap(
    unpeel_native_bridge_remote_handle_t handle,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Poll a bounded output page. Metadata is owned UTF-8 JSON with sessionID,
 * requestedOffset, offset, nextOffset, resetBeforeFeed, truncated,
 * capturedAtUnixMs, and byteCount; terminal bytes are a separate owned binary
 * buffer. The returned page must be committed after rendering or discarded.
 * On failure page and bytes are empty and metadata may contain owned error JSON.
 */
int32_t unpeel_native_bridge_remote_output_poll(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    size_t limit,
    uint64_t wait_ms,
    unpeel_native_bridge_remote_output_page_handle_t *out_page_handle,
    uint8_t **out_metadata_pointer,
    size_t *out_metadata_length,
    uint8_t **out_bytes_pointer,
    size_t *out_bytes_length
);

/*
 * Poll from the Controller's exact rendered cursor. has_offset == 0 requests
 * a fresh bounded tail; otherwise requested_offset is sent verbatim. This
 * atomically supersedes any older pending page for the Session.
 */
int32_t unpeel_native_bridge_remote_output_poll_from(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint64_t requested_offset,
    uint8_t has_offset,
    size_t limit,
    uint64_t wait_ms,
    unpeel_native_bridge_remote_output_page_handle_t *out_page_handle,
    uint8_t **out_metadata_pointer,
    size_t *out_metadata_length,
    uint8_t **out_bytes_pointer,
    size_t *out_bytes_length
);

/* Commit after every output byte was accepted; advances the cursor once. */
int32_t unpeel_native_bridge_remote_output_commit(
    unpeel_native_bridge_remote_handle_t handle,
    unpeel_native_bridge_remote_output_page_handle_t page_handle,
    uint8_t **out_pointer,
    size_t *out_length
);

/* Explicitly discard a staged page without advancing its cursor. */
int32_t unpeel_native_bridge_remote_output_discard(
    unpeel_native_bridge_remote_handle_t handle,
    unpeel_native_bridge_remote_output_page_handle_t page_handle,
    uint8_t **out_pointer,
    size_t *out_length
);

/* Reset one Session to a fresh bounded tail and discard its staged page. */
int32_t unpeel_native_bridge_remote_output_reset(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * The following generation-bound effects are sent at most once and are never
 * automatically replayed. Success returns owned {"requestID":...} JSON.
 * Failure returns owned JSON with kind (notApplied/outcomeUnknown), code,
 * operation, and message.
 */
int32_t unpeel_native_bridge_remote_terminal_write(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    const uint8_t *data_pointer,
    size_t data_length,
    uint8_t **out_pointer,
    size_t *out_length
);

int32_t unpeel_native_bridge_remote_desktop_fit(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint16_t columns,
    uint16_t rows,
    uint8_t **out_pointer,
    size_t *out_length
);

int32_t unpeel_native_bridge_remote_desktop_clear(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

int32_t unpeel_native_bridge_remote_mark_read(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Session organization/lifecycle effects. Same at-most-once contract and
 * JSON envelopes as the terminal effects above.
 */
int32_t unpeel_native_bridge_remote_session_title_set(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    const uint8_t *title_pointer,
    size_t title_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * File a remote Session under another project/group (session.project.set)
 * via the Host's shared project-override marker.
 */
int32_t unpeel_native_bridge_remote_session_project_set(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    const uint8_t *project_id_pointer,
    size_t project_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/* pinned is non-zero to pin, zero to unpin. */
int32_t unpeel_native_bridge_remote_session_pinned_set(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    int32_t pinned,
    uint8_t **out_pointer,
    size_t *out_length
);

/* enabled is non-zero to opt in, zero to opt out. */
int32_t unpeel_native_bridge_remote_session_notify_when_done_set(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    int32_t enabled,
    uint8_t **out_pointer,
    size_t *out_length
);

/* approved is non-zero to allow, zero to deny. */
int32_t unpeel_native_bridge_remote_approval_answer(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *approval_id_pointer,
    size_t approval_id_length,
    int32_t approved,
    uint8_t **out_pointer,
    size_t *out_length
);

int32_t unpeel_native_bridge_remote_session_archive(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

int32_t unpeel_native_bridge_remote_session_restore(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

int32_t unpeel_native_bridge_remote_session_stop(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

int32_t unpeel_native_bridge_remote_session_remove(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

int32_t unpeel_native_bridge_remote_session_restart(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

int32_t unpeel_native_bridge_remote_session_restart_agent(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

int32_t unpeel_native_bridge_remote_session_resume_agent(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Replace one project's hand-ordered Session ranks. ordered_ids_json is a
 * UTF-8 JSON array of Session id strings (combined pinned + regular order).
 */
int32_t unpeel_native_bridge_remote_session_order_set(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *project_id_pointer,
    size_t project_id_length,
    const uint8_t *ordered_ids_json_pointer,
    size_t ordered_ids_json_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Edit the remote Host's flat preset list (settings.presets.set). patch_json
 * is the camelCase one-preset patch (presetID?, command?, label?,
 * quickLaunch?, sortOrder?, removed?). No presetID creates (command
 * required, the Host mints the id); removed deletes and cannot combine with
 * other fields; sortOrder moves the preset to that index in the Host's
 * display order.
 */
int32_t unpeel_native_bridge_remote_preset_set(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *patch_json_pointer,
    size_t patch_json_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Edit the remote workspace's behavior knobs (settings.workspace.set).
 * patch_json is the camelCase typed patch; absent fields are left unchanged
 * and every present field is validated before anything applies.
 */
int32_t unpeel_native_bridge_remote_workspace_settings_set(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *patch_json_pointer,
    size_t patch_json_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/* Set one Host-owned typed resource opener preference (settings.openers.set). */
int32_t unpeel_native_bridge_remote_opener_set(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *body_json_pointer,
    size_t body_json_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/* Install one official App on the Host (apps.install). */
int32_t unpeel_native_bridge_remote_app_install(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *body_json_pointer,
    size_t body_json_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/* Open one installed App through the Host's semantic presentation path. */
int32_t unpeel_native_bridge_remote_app_open(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *body_json_pointer,
    size_t body_json_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Organize one remote project/group (project.organization.set). patch_json
 * is the camelCase one-project patch (sortOrder?, displayName?, colorID?,
 * dateSorted?; absent fields are left unchanged). sortOrder moves the
 * project to that index among its same-parent siblings in the Host's
 * current display order.
 */
int32_t unpeel_native_bridge_remote_project_organization_set(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *project_id_pointer,
    size_t project_id_length,
    const uint8_t *patch_json_pointer,
    size_t patch_json_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Create a Session on the Host from a user-initiated Controller action.
 * request_json carries projectID, presetID?, command?, worktreePath?,
 * worktreeBranch?, initialText?, initialTextSubmitMode?. Success returns
 * owned JSON with requestID, sessionID, capturedAtUnixMs?, and session?.
 */
int32_t unpeel_native_bridge_remote_session_create(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *request_json_pointer,
    size_t request_json_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Create or complete a controller-assisted pairing invitation. request_json
 * carries {action:"create", endpoint:...} or {action:"complete", envelope:...}.
 * Success returns the Host's raw JSON pairing payload/envelope.
 */
int32_t unpeel_native_bridge_remote_pairing_invitation(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *request_json_pointer,
    size_t request_json_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Upload raw image bytes to the selected Host (artifact.upload). An empty
 * session id targets the Host's shared dropped-images dir. Success returns
 * owned JSON {path} naming the HOST-side file to paste as an attachable
 * reference; failures use the effect error envelope (never auto-replayed).
 */
int32_t unpeel_native_bridge_remote_upload_attachment(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    const uint8_t *content_type_pointer,
    size_t content_type_length,
    const uint8_t *bytes_pointer,
    size_t bytes_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * Capability-gated reads (not effects). Success returns owned typed JSON;
 * failures return owned JSON with stable code and error fields.
 */
int32_t unpeel_native_bridge_remote_archived_sessions(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *project_id_pointer,
    size_t project_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/* entries limits to the most recent N transcript entries; 0 = Host default. */
int32_t unpeel_native_bridge_remote_transcript_markdown(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint32_t entries,
    uint8_t **out_pointer,
    size_t *out_length
);

/*
 * One live Session's current terminal grid. Success returns owned JSON with
 * sessionID, columns, rows, outputOffset?, and capturedAtUnixMs.
 */
int32_t unpeel_native_bridge_remote_session_metrics(
    unpeel_native_bridge_remote_handle_t handle,
    const uint8_t *session_id_pointer,
    size_t session_id_length,
    uint8_t **out_pointer,
    size_t *out_length
);

/* Remove the handle, discard all owned pages, and disconnect its SSH process. */
int32_t unpeel_native_bridge_remote_close(
    unpeel_native_bridge_remote_handle_t handle,
    uint8_t **out_pointer,
    size_t *out_length
);

void unpeel_native_bridge_free(uint8_t *pointer, size_t length);

#endif
