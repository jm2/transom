// SPDX-License-Identifier: Apache-2.0
// transom-view from Vala. Build and run:
//   valac --pkg gtk4 --pkg transom-view-0 --vapidir build/capi/share/vala/vapi \
//       -X -Ibuild/capi/include/transom-view-0 -X -Lbuild/capi/lib -X -ltransom-view-0 \
//       examples/vala/transom-view-demo.vala -o build/transom-view-demo-vala
//   LD_LIBRARY_PATH=build/capi/lib build/transom-view-demo-vala HOST PORT USER PASSWORD_FILE [SHA256]

int main (string[] args) {
    if (args.length < 5) {
        stderr.printf ("usage: %s HOST PORT USER PASSWORD_FILE [SHA256]\n", args[0]);
        return 2;
    }
    string host = args[1];
    uint16 port = (uint16) int.parse (args[2]);
    string user = args[3];
    string password;
    try {
        FileUtils.get_contents (args[4], out password);
    } catch (Error e) {
        stderr.printf ("%s\n", e.message);
        return 2;
    }
    password = password.strip ();
    string? trust = args.length > 5 ? args[5] : null;

    var app = new Gtk.Application ("io.github.jm2.Transom.ValaDemo", ApplicationFlags.NON_UNIQUE);
    app.activate.connect (() => {
        var win = new Gtk.ApplicationWindow (app);
        win.title = "transom-view (Vala)";
        win.set_default_size (1100, 700);
        var view = new Transom.View ();
        win.child = view;
        view.certificate_prompt.connect ((h, p, subject, issuer, fp, prev) => {
            print ("certificate for %s:%u: %s\n", h, p, fp);
            view.answer_certificate (fp == trust ? Transom.CertDecision.ACCEPT_ONCE
                                                 : Transom.CertDecision.REJECT);
        });
        view.desktop_resized.connect ((w, h) => print ("desktop %ux%u\n", w, h));
        view.disconnected.connect ((reason) => { print ("disconnected: %s\n", reason); app.quit (); });
        view.connected.connect (() => {
            print ("connected: %s\n", view.is_connected ().to_string ());
            Timeout.add_seconds (4, () => { view.shutdown (5000); app.quit (); return false; });
        });
        win.present ();
        Timeout.add (300, () => {
            try {
                view.connect_to (host, port, user, null, password);
            } catch (Error e) {
                print ("connect failed: %s\n", e.message);
                app.quit ();
            }
            return false;
        });
    });
    return app.run (null);
}
