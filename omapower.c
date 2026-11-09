#include "omapower.h"
#include <glib-unix.h>
#include "json-util.h"
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/file.h>
#include <unistd.h>

/* Unity build: compile this file only. */
#include "config.c"
#include "process-watch.c"

/* Exit status when another instance holds the per-user lock (EX_TEMPFAIL). */
#define EXIT_BUSY 75

#define DEST "net.hadess.PowerProfiles"
#define OBJECT "/net/hadess/PowerProfiles"

typedef enum { IDLE, ACQUIRING, RELEASING } Operation;

typedef struct {
    GDBusConnection *bus;
    GMainLoop *loop;
    ProcessWatch *processes;
    ConfigWatch *config_watch;
    GHashTable *commands; /* configured names, for status reports */
    gchar *config, *last_status, *owner;
    GCancellable *cancel;
    guint generation, cookie, released_cookie, name_watch, subscription;
    guint reconcile_source, retry_source, stdin_source;
    guint release_failures;
    Operation pending;
    gboolean holding, suppressed, rejected, stopping, release_seen;
    gboolean enabled, config_error, supervised;
    int exit_status;
} App;

static void reconcile(App *app);

static void fail(App *app, const char *message) {
    g_printerr("omapower: %s\n", message);
    app->exit_status = 1;
    g_main_loop_quit(app->loop);
}

#define BOOL(value) ((value) ? "true" : "false")

/* Report state once per change: a log line, plus one JSON line on stdout when
 * supervised by the shell plugin. Returns FALSE if the supervisor has gone. */
static gboolean report_status(App *app) {
    GHashTable *active = process_watch_active(app->processes);
    gchar *names = json_names(active);
    gchar *commands = json_names(app->commands);
    char *mode = json_quote(process_watch_mode(app->processes));
    if (!mode) g_error("cannot encode JSON detection mode");
    gchar *json = g_strdup_printf("{\"enabled\":%s,\"configError\":%s,\"commands\":%s,\"active\":%s,"
        "\"holding\":%s,\"suppressed\":%s,\"rejected\":%s,\"available\":%s,\"monitor\":%s}",
        BOOL(app->enabled), BOOL(app->config_error), commands, names, BOOL(app->holding),
        BOOL(app->suppressed), BOOL(app->rejected), BOOL(app->owner), mode);
    g_free(names);
    g_free(commands);
    free(mode);
    g_hash_table_unref(active);
    gboolean ok = TRUE;
    if (g_strcmp0(json, app->last_status)) {
        g_printerr("omapower: state %s\n", json);
        if (app->supervised) ok = printf("%s\n", json) > 0 && fflush(stdout) == 0;
        g_free(app->last_status);
        app->last_status = json;
    } else g_free(json);
    return ok;
}

static gboolean reconcile_idle(gpointer data) {
    App *app = data;
    app->reconcile_source = 0;
    reconcile(app);
    return G_SOURCE_REMOVE;
}

static void schedule_reconcile(App *app) {
    if (!app->stopping && !app->reconcile_source)
        app->reconcile_source = g_idle_add_full(G_PRIORITY_DEFAULT, reconcile_idle, app, NULL);
}

static gboolean retry_release(gpointer data) {
    App *app = data;
    app->retry_source = 0;
    schedule_reconcile(app);
    return G_SOURCE_REMOVE;
}

typedef struct { App *app; guint generation; Operation operation; } PendingCall;

/* Errors the daemon itself returns when it refuses a hold. Anything else
 * (timeouts, disconnects) leaves it unknown whether the hold was created. */
static gboolean definite_rejection(const GError *error) {
    return g_error_matches(error, G_DBUS_ERROR, G_DBUS_ERROR_INVALID_ARGS) ||
           g_error_matches(error, G_DBUS_ERROR, G_DBUS_ERROR_ACCESS_DENIED) ||
           g_error_matches(error, G_DBUS_ERROR, G_DBUS_ERROR_AUTH_FAILED);
}

static void call_finished(GObject *connection, GAsyncResult *result, gpointer data) {
    PendingCall *call = data;
    App *app = call->app;
    GError *error = NULL;
    GVariant *reply = g_dbus_connection_call_finish(G_DBUS_CONNECTION(connection), result, &error);
    app->pending = IDLE;
    g_clear_object(&app->cancel);
    if (!app->stopping && call->generation == app->generation) {
        if (reply) {
            if (call->operation == ACQUIRING) {
                g_variant_get(reply, "(u)", &app->cookie);
                app->holding = !(app->release_seen && app->released_cookie == app->cookie);
                g_printerr("omapower: hold acquired: cookie=%u retained=%s\n",
                           app->cookie, app->holding ? "yes" : "no (already released by daemon)");
                if (!app->holding) {
                    app->cookie = 0;
                    app->suppressed = process_watch_running(app->processes);
                }
            } else {
                g_printerr("omapower: hold release confirmed\n");
                app->holding = FALSE;
                app->cookie = 0;
                app->release_failures = 0;
            }
        } else if (call->operation == RELEASING && app->holding) {
            /* A failed/ambiguous release is NOT a successful release. Retain
             * the cookie; disconnect after bounded retries to drop all holds. */
            g_printerr("omapower: release: %s\n", error->message);
            if (++app->release_failures >= 3) fail(app, "release failed three times; disconnecting to drop the hold");
            else app->retry_source = g_timeout_add_seconds(app->release_failures, retry_release, app);
        } else if (call->operation == ACQUIRING && definite_rejection(error)) {
            /* The daemon answered "no" (e.g. no performance profile on this
             * machine), so no hold exists. Don't retry or restart in a loop:
             * wait until the matching commands have all exited. */
            g_printerr("omapower: performance hold rejected: %s\n", error->message);
            app->rejected = TRUE;
        } else if (call->operation == ACQUIRING) {
            /* A timed-out acquisition may still have succeeded remotely.
             * Never retry it on the same connection and leak another cookie. */
            fail(app, error->message);
        }
    }
    if (reply) g_variant_unref(reply);
    g_clear_error(&error);
    g_free(call);
    schedule_reconcile(app);
}

static void start_call(App *app, Operation operation) {
    g_printerr("omapower: %s: owner=%s cookie=%u\n",
               operation == ACQUIRING ? "requesting performance hold" : "releasing performance hold",
               app->owner, app->cookie);
    app->pending = operation;
    if (operation == ACQUIRING) {
        app->release_seen = FALSE;
        app->release_failures = 0;
    }
    app->cancel = g_cancellable_new();
    PendingCall *call = g_new(PendingCall, 1);
    *call = (PendingCall){ app, app->generation, operation };
    g_dbus_connection_call(app->bus, app->owner, OBJECT, DEST,
        operation == ACQUIRING ? "HoldProfile" : "ReleaseProfile",
        operation == ACQUIRING ? g_variant_new("(sss)", "performance", "Whitelisted command running", "omapower")
                              : g_variant_new("(u)", app->cookie),
        operation == ACQUIRING ? G_VARIANT_TYPE("(u)") : G_VARIANT_TYPE_UNIT,
        G_DBUS_CALL_FLAGS_NONE, 5000, app->cancel, call_finished, call);
}

static void reconcile(App *app) {
    gboolean running = process_watch_running(app->processes);
    if (!running) app->suppressed = app->rejected = FALSE;
    gboolean wanted = app->enabled && running && !app->suppressed && !app->rejected;
    if (wanted && app->holding && app->retry_source) {
        remove_source(&app->retry_source);
        app->release_failures = 0;
    }
    if (app->pending == IDLE && app->owner && !app->retry_source) {
        if (wanted && !app->holding) start_call(app, ACQUIRING);
        else if (!wanted && app->holding) start_call(app, RELEASING);
    }
    if (!report_status(app)) fail(app, "status output closed");
}

static void processes_changed(ProcessWatch *watch, const GError *error, gpointer data) {
    (void)watch;
    App *app = data;
    if (error) fail(app, error->message);
    else schedule_reconcile(app);
}

static void owner_changed(App *app, const char *owner) {
    if (app->stopping || !g_strcmp0(app->owner, owner)) return;
    g_printerr("omapower: power-profiles owner: %s -> %s\n",
               app->owner ? app->owner : "unavailable", owner ? owner : "unavailable");
    g_free(app->owner);
    app->owner = g_strdup(owner);
    app->generation++;
    app->holding = FALSE;
    app->rejected = FALSE;
    app->cookie = 0;
    app->release_failures = 0;
    remove_source(&app->retry_source);
    if (app->cancel) g_cancellable_cancel(app->cancel);
    schedule_reconcile(app);
}

static void appeared(GDBusConnection *connection, const gchar *name, const gchar *owner, gpointer data) {
    (void)connection; (void)name;
    owner_changed(data, owner);
}

static void vanished(GDBusConnection *connection, const gchar *name, gpointer data) {
    (void)connection; (void)name;
    owner_changed(data, NULL);
}

static void bus_closed(GDBusConnection *connection, gboolean remote, GError *error, gpointer data) {
    (void)connection; (void)remote;
    App *app = data;
    if (!app->stopping) fail(app, error ? error->message : "system bus disconnected");
}

static void profile_released(GDBusConnection *connection, const gchar *sender, const gchar *path,
                             const gchar *interface, const gchar *signal_name,
                             GVariant *parameters, gpointer data) {
    (void)connection; (void)path; (void)interface; (void)signal_name;
    App *app = data;
    if (app->stopping || g_strcmp0(sender, app->owner)
        || !g_variant_is_of_type(parameters, G_VARIANT_TYPE("(u)"))) return;
    guint cookie;
    g_variant_get(parameters, "(u)", &cookie);
    g_printerr("omapower: ProfileReleased: cookie=%u held-cookie=%u pending=%s\n", cookie, app->cookie,
               app->pending == ACQUIRING ? "acquire" : app->pending == RELEASING ? "release" : "none");
    if (app->pending == ACQUIRING) {
        app->released_cookie = cookie;
        app->release_seen = TRUE;
    }
    if (app->holding && app->cookie == cookie) {
        app->holding = FALSE;
        app->cookie = 0;
        if (app->pending != RELEASING) app->suppressed = process_watch_running(app->processes);
        remove_source(&app->retry_source);
        schedule_reconcile(app);
    }
}

/* An invalid configuration watches nothing (dropping any hold) and is
 * reported, so it can be fixed without restarting Omapower. */
static GHashTable *read_configuration(App *app) {
    GError *error = NULL;
    gboolean enabled = TRUE;
    GHashTable *commands = load_config(app->config, &enabled, &error);
    app->config_error = !commands;
    app->enabled = commands ? enabled : FALSE;
    if (!commands) {
        g_printerr("omapower: %s: %s; watching nothing until it is fixed\n", app->config, error->message);
        g_error_free(error);
        commands = g_hash_table_new_full(g_str_hash, g_str_equal, g_free, NULL);
    }
    if (app->commands) g_hash_table_unref(app->commands);
    app->commands = g_hash_table_ref(commands);
    return commands;
}

static void config_changed(const char *path, const GError *monitor_error, gpointer data) {
    (void)path;
    App *app = data;
    if (app->stopping) return;
    if (monitor_error) { fail(app, monitor_error->message); return; }
    GError *error = NULL;
    GHashTable *commands = read_configuration(app);
    process_watch_set_commands(app->processes, commands, &error);
    g_hash_table_unref(commands);
    if (error) { fail(app, error->message); g_error_free(error); }
    else schedule_reconcile(app);
}

/* Supervised mode: the shell plugin holds our stdin. EOF means it is gone. */
static gboolean stdin_ready(gint fd, GIOCondition condition, gpointer data) {
    App *app = data;
    char buffer[256];
    ssize_t count = (condition & G_IO_IN) ? read(fd, buffer, sizeof buffer) : 0;
    if (count > 0 || (count < 0 && (errno == EINTR || errno == EAGAIN))) return G_SOURCE_CONTINUE;
    g_printerr("omapower: supervisor closed the pipe; exiting\n");
    app->stdin_source = 0;
    g_main_loop_quit(app->loop);
    return G_SOURCE_REMOVE;
}

/* One daemon per user, however it was started. Wait briefly so a restart can
 * replace an instance that is still releasing its hold. */
static int acquire_lock(void) {
    gchar *path = g_build_filename(g_get_user_runtime_dir(), "omapower.lock", NULL);
    int fd = open(path, O_CREAT | O_RDWR | O_CLOEXEC, 0600);
    g_free(path);
    if (fd < 0) {
        g_printerr("omapower: lock: %s\n", g_strerror(errno));
        return -1;
    }
    for (int attempt = 0; attempt < 50; attempt++) {
        if (flock(fd, LOCK_EX | LOCK_NB) == 0) return fd;
        if (errno != EWOULDBLOCK && errno != EINTR) break;
        g_usleep(100000);
    }
    close(fd);
    return -1;
}

static gboolean stop(gpointer data) {
    g_main_loop_quit(((App *)data)->loop);
    return G_SOURCE_CONTINUE; /* cleanup removes the signal sources */
}

static int usage(void) {
    g_printerr("Usage: omapower [--supervised] [--poll]\n"
               "       omapower --add NAME | --remove NAME | --enable | --disable\n");
    return 2;
}

int main(int argc, char **argv) {
    if (argc == 3 && (!strcmp(argv[1], "--add") || !strcmp(argv[1], "--remove")))
        return edit_command(argv[1], argv[2]);
    if (argc == 2 && (!strcmp(argv[1], "--enable") || !strcmp(argv[1], "--disable")))
        return edit_command(argv[1], NULL);
    App app = {0};
    gboolean force_poll = FALSE;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--poll") && !force_poll) force_poll = TRUE;
        else if (!strcmp(argv[i], "--supervised") && !app.supervised) app.supervised = TRUE;
        else return usage();
    }
    int lock = acquire_lock();
    if (lock < 0) {
        g_printerr("omapower: another Omapower instance is already running\n");
        return EXIT_BUSY;
    }
    GError *error = NULL;
    guint term = 0, interrupt = 0;
    if (app.supervised) signal(SIGPIPE, SIG_IGN); /* report a closed stdout as an error */
    app.config = g_build_filename(g_get_user_config_dir(), "omapower", "config.json", NULL);
    app.loop = g_main_loop_new(NULL, FALSE);
    app.config_watch = config_watch_new(app.config, config_changed, &app, &error);
    if (!app.config_watch) goto failure;
    GHashTable *commands = read_configuration(&app);
    app.processes = process_watch_new(commands, force_poll, processes_changed, &app, &error);
    g_hash_table_unref(commands);
    if (!app.processes) goto failure;
    app.bus = g_bus_get_sync(G_BUS_TYPE_SYSTEM, NULL, &error);
    if (!app.bus) goto failure;
    g_dbus_connection_set_exit_on_close(app.bus, FALSE);
    g_signal_connect(app.bus, "closed", G_CALLBACK(bus_closed), &app);
    app.subscription = g_dbus_connection_signal_subscribe(app.bus, DEST, DEST, "ProfileReleased", OBJECT,
        NULL, G_DBUS_SIGNAL_FLAGS_NONE, profile_released, &app, NULL);
    app.name_watch = g_bus_watch_name_on_connection(app.bus, DEST, G_BUS_NAME_WATCHER_FLAGS_AUTO_START,
                                                   appeared, vanished, &app, NULL);
    term = g_unix_signal_add(SIGTERM, stop, &app);
    interrupt = g_unix_signal_add(SIGINT, stop, &app);
    if (app.supervised)
        app.stdin_source = g_unix_fd_add(STDIN_FILENO, G_IO_IN | G_IO_HUP | G_IO_ERR, stdin_ready, &app);
    schedule_reconcile(&app);
    g_main_loop_run(app.loop);
    goto done;
failure:
    fail(&app, error ? error->message : "initialization failed");
    g_clear_error(&error);
done:
    app.stopping = TRUE;
    remove_source(&term);
    remove_source(&interrupt);
    remove_source(&app.reconcile_source);
    remove_source(&app.retry_source);
    remove_source(&app.stdin_source);
    config_watch_free(app.config_watch);
    process_watch_free(app.processes);
    if (app.name_watch) g_bus_unwatch_name(app.name_watch);
    if (app.subscription) g_dbus_connection_signal_unsubscribe(app.bus, app.subscription);
    if (app.cancel) g_cancellable_cancel(app.cancel);
    if (app.bus) {
        /* Holds belong to this connection. Closing it also cleans up a hold
         * whose acquisition/release reply was lost or cancelled. */
        g_dbus_connection_close_sync(app.bus, NULL, NULL);
        while (app.pending != IDLE) g_main_context_iteration(NULL, TRUE);
        g_object_unref(app.bus);
    }
    g_main_loop_unref(app.loop);
    if (app.commands) g_hash_table_unref(app.commands);
    g_free(app.config); g_free(app.last_status); g_free(app.owner);
    close(lock);
    return app.exit_status;
}
