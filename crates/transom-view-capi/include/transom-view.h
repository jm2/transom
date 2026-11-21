/* SPDX-License-Identifier: Apache-2.0 */
/*
 * transom-view: a HiDPI-correct GTK 4 RDP display widget.
 *
 * C API (ABI version 0). GObject introspection data (TransomView-0.gir/.typelib) and
 * a Vala binding (transom-view-0.vapi) are generated from this header.
 */
#pragma once

#include <gtk/gtk.h>

G_BEGIN_DECLS

/**
 * TransomCertDecision:
 * @TRANSOM_CERT_REJECT: do not connect
 * @TRANSOM_CERT_ACCEPT_ONCE: accept for this connection only
 * @TRANSOM_CERT_ACCEPT_AND_REMEMBER: accept; the application stores the fingerprint
 *
 * Answer to #TransomView::certificate-prompt.
 */
typedef enum {
  TRANSOM_CERT_REJECT = 0,
  TRANSOM_CERT_ACCEPT_ONCE = 1,
  TRANSOM_CERT_ACCEPT_AND_REMEMBER = 2,
} TransomCertDecision;

#define TRANSOM_TYPE_VIEW (transom_view_get_type ())
G_DECLARE_FINAL_TYPE (TransomView, transom_view, TRANSOM, VIEW, GtkWidget)

/**
 * transom_view_new:
 *
 * Creates a view. Signals: `connected`, `disconnected` (reason),
 * `desktop-resized` (width, height), `certificate-prompt` (host, port, subject,
 * issuer, sha256, previous sha256 or ""), `credentials-required` (username,
 * domain).
 *
 * Returns: (transfer floating): a new #TransomView
 */
GtkWidget *transom_view_new (void);

/**
 * transom_view_connect_to:
 * @self: a #TransomView
 * @host: server host name or address
 * @port: server port (3389 if 0)
 * @username: (nullable): user name
 * @domain: (nullable): domain
 * @password: (nullable): password; if %NULL, #TransomView::credentials-required is
 *   emitted when the server asks
 * @error: return location for a #GError
 *
 * Starts connecting. The remote desktop is sized to the widget in device pixels and
 * told the local scale, so it renders pixel-exact.
 *
 * Returns: %TRUE if the connection attempt started
 */
gboolean transom_view_connect_to (TransomView *self,
                                  const char  *host,
                                  guint16      port,
                                  const char  *username,
                                  const char  *domain,
                                  const char  *password,
                                  GError     **error);

/**
 * transom_view_disconnect:
 * @self: a #TransomView
 *
 * Disconnects without blocking.
 */
void transom_view_disconnect (TransomView *self);

/**
 * transom_view_shutdown:
 * @self: a #TransomView
 * @timeout_ms: how long to wait
 *
 * Disconnects and waits for the backend to stop. Call before the process exits.
 *
 * Returns: %TRUE if it stopped within @timeout_ms
 */
gboolean transom_view_shutdown (TransomView *self, guint timeout_ms);

/**
 * transom_view_is_connected:
 * @self: a #TransomView
 *
 * Returns: whether a session is up
 */
gboolean transom_view_is_connected (TransomView *self);

/**
 * transom_view_send_ctrl_alt_del:
 * @self: a #TransomView
 *
 * Sends Ctrl+Alt+Del to the remote computer.
 */
void transom_view_send_ctrl_alt_del (TransomView *self);

/**
 * transom_view_answer_certificate:
 * @self: a #TransomView
 * @decision: the user's decision
 *
 * Answers the pending #TransomView::certificate-prompt.
 */
void transom_view_answer_certificate (TransomView *self, TransomCertDecision decision);

/**
 * transom_view_answer_credentials:
 * @self: a #TransomView
 * @username: user name
 * @domain: (nullable): domain
 * @password: password
 *
 * Answers the pending #TransomView::credentials-required.
 */
void transom_view_answer_credentials (TransomView *self,
                                      const char  *username,
                                      const char  *domain,
                                      const char  *password);

/**
 * transom_view_cancel_credentials:
 * @self: a #TransomView
 *
 * Cancels the pending #TransomView::credentials-required (the connection fails).
 */
void transom_view_cancel_credentials (TransomView *self);

G_END_DECLS
