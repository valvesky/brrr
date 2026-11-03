#include "omapower.h"

#include "json-util.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <unistd.h>

static struct json_value_s *read_config(const char *path, GError **error) {
    gchar *contents = NULL;
    gsize length = 0;
    if (!g_file_get_contents(path, &contents, &length, error)) return NULL;
    struct json_parse_result_s result = {0};
    struct json_value_s *root = NULL;
    if (g_utf8_validate(contents, length, NULL))
        root = json_parse_ex(contents, length, json_parse_flags_default, NULL, NULL, &result);
    g_free(contents);
    if (!root)
        g_set_error(error, G_FILE_ERROR, G_FILE_ERROR_INVAL,
                    "invalid JSON configuration (error %zu at byte %zu)", result.error, result.error_offset);
    return root;
}

static GHashTable *commands_from_json(struct json_value_s *root, GError **error) {
    GHashTable *names = g_hash_table_new_full(g_str_hash, g_str_equal, g_free, NULL);
    struct json_value_s *node = json_member(root, "commands");
    struct json_array_s *array = node ? json_value_as_array(node) : NULL;
    if (!array) {
        g_set_error_literal(error, G_FILE_ERROR, G_FILE_ERROR_INVAL, "commands must be an array of process names");
        goto fail;
    }
    for (struct json_array_element_s *p = array->start; p; p = p->next) {
        struct json_string_s *string = json_value_as_string(p->value);
        if (!string) {
            g_set_error_literal(error, G_FILE_ERROR, G_FILE_ERROR_INVAL, "commands must contain strings");
            goto fail;
        }
        const char *name = string->string;
        if (!string->string_size || string->string_size > 15 || strlen(name) != string->string_size
            || strchr(name, '/') || strchr(name, '\n') || !g_utf8_validate(name, string->string_size, NULL)) {
            g_set_error_literal(error, G_FILE_ERROR, G_FILE_ERROR_INVAL, "process names must be 1–15 bytes without NULs, slashes or newlines");
            goto fail;
        }
        g_hash_table_add(names, g_strdup(name));
    }
    return names;
fail:
    g_hash_table_unref(names);
    return NULL;
}

static GHashTable *default_commands(void) {
    GHashTable *names = g_hash_table_new_full(g_str_hash, g_str_equal, g_free, NULL);
    /* Include GCC worker processes as well as compiler drivers. */
    const char *defaults[] = {
        "rustc", "cargo", "gcc", "g++", "cc", "c++", "cc1", "cc1plus",
        "clang", "clang++", "zig", "go", "javac", "swiftc", "swift-frontend",
        "gfortran", "flang", "nvcc", "ghc"
    };
    for (guint i = 0; i < G_N_ELEMENTS(defaults); i++)
        g_hash_table_add(names, g_strdup(defaults[i]));
    return names;
}

/* A missing "enabled" member means automatic boost is on. */
static gboolean enabled_from_json(struct json_value_s *root, gboolean *enabled, GError **error) {
    struct json_value_s *node = json_member(root, "enabled");
    if (!node || json_value_is_true(node)) *enabled = TRUE;
    else if (json_value_is_false(node)) *enabled = FALSE;
    else {
        g_set_error_literal(error, G_FILE_ERROR, G_FILE_ERROR_INVAL, "enabled must be true or false");
        return FALSE;
    }
    return TRUE;
}

GHashTable *load_config(const char *path, gboolean *enabled, GError **error) {
    /* Use a local error too: callers may pass NULL for their error output. */
    GError *local_error = NULL;
    gboolean on = TRUE;
    struct json_value_s *root = read_config(path, &local_error);
    if (!root) {
        if (g_error_matches(local_error, G_FILE_ERROR, G_FILE_ERROR_NOENT)) {
            g_clear_error(&local_error);
            if (enabled) *enabled = TRUE;
            return default_commands();
        }
        g_propagate_error(error, local_error);
        return NULL;
    }
    GHashTable *names = enabled_from_json(root, &on, error) ? commands_from_json(root, error) : NULL;
    free(root);
    if (names && enabled) *enabled = on;
    return names;
}

GHashTable *load_commands(const char *path, GError **error) {
    return load_config(path, NULL, error);
}

static gboolean member_is(const struct json_object_element_s *p, const char *name) {
    return p->name->string_size == strlen(name) && !memcmp(p->name->string, name, strlen(name));
}

/* Preserve unrelated fields, including nested values and exact number tokens.
 * enabled < 0 keeps the existing "enabled" member unchanged. */
static gchar *encode_config(struct json_value_s *root, GHashTable *names, int enabled) {
    gchar *commands = json_names(names);
    GString *output = g_string_new("{\"commands\":");
    g_string_append(output, commands);
    g_free(commands);
    if (enabled >= 0) g_string_append(output, enabled ? ",\"enabled\":true" : ",\"enabled\":false");
    struct json_object_s *object = json_value_as_object(root);
    for (struct json_object_element_s *p = object->start; p; p = p->next) {
        if (member_is(p, "commands") || (enabled >= 0 && member_is(p, "enabled"))) continue;
        struct json_value_s key = { p->name, json_type_string };
        char *name = json_write_minified(&key, NULL);
        char *value = json_write_minified(p->value, NULL);
        if (!name || !value) g_error("cannot encode JSON configuration");
        g_string_append_printf(output, ",%s:%s", name, value);
        free(name);
        free(value);
    }
    g_string_append_c(output, '}');
    return g_string_free(output, FALSE);
}

int edit_command(const char *action, const char *name) {
    GError *error = NULL;
    int enabled = !strcmp(action, "--enable") ? 1 : !strcmp(action, "--disable") ? 0 : -1;
    if (enabled < 0 && (!name || !*name || strlen(name) > 15 || strchr(name, '/') || strchr(name, '\n')
                        || !g_utf8_validate(name, -1, NULL))) {
        g_printerr("omapower: invalid command name\n");
        return 1;
    }
    gchar *dir = g_build_filename(g_get_user_config_dir(), "omapower", NULL);
    gchar *path = g_build_filename(dir, "config.json", NULL);
    gchar *lockpath = g_build_filename(dir, "config.lock", NULL);
    int fd = -1, rc = 1;
    GHashTable *names = NULL;
    struct json_value_s *root = NULL;
    gchar *output = NULL;
    if (g_mkdir_with_parents(dir, 0700) != 0 || (fd = open(lockpath, O_CREAT | O_RDWR | O_CLOEXEC, 0600)) < 0 || flock(fd, LOCK_EX) != 0) {
        g_printerr("omapower: config lock: %s\n", g_strerror(errno));
        goto done;
    }
    root = read_config(path, &error);
    if (!root && g_error_matches(error, G_FILE_ERROR, G_FILE_ERROR_NOENT)) {
        g_clear_error(&error);
        names = default_commands();
        root = json_parse("{}", 2);
        if (!root) g_error("cannot allocate JSON configuration");
    } else if (root) {
        /* Refuse to rewrite a configuration that is invalid in any field. */
        gboolean ignored;
        if (enabled_from_json(root, &ignored, &error)) names = commands_from_json(root, &error);
    }
    if (!names) goto failure;
    if (strcmp(action, "--add") == 0) g_hash_table_add(names, g_strdup(name));
    else if (strcmp(action, "--remove") == 0) g_hash_table_remove(names, name);
    output = encode_config(root, names, enabled);
    if (!g_file_set_contents(path, output, -1, &error)) goto failure;
    rc = 0;
    goto done;
failure:
    g_printerr("omapower: %s\n", error ? error->message : "cannot update config");
    g_clear_error(&error);
done:
    g_free(output);
    free(root);
    if (names) g_hash_table_unref(names);
    if (fd >= 0) close(fd);
    g_free(lockpath); g_free(path); g_free(dir);
    return rc;
}

struct ConfigWatch {
    gchar *path;
    GFile *file, *directory, *target;
    GFileMonitor *monitor, *parent, *target_monitor;
    guint reload_source;
    gboolean rearm;
    ConfigChanged changed;
    gpointer data;
};

static void changed(GFileMonitor *monitor, GFile *file, GFile *other,
                    GFileMonitorEvent event, gpointer data);

static gchar *resolve_target(const char *path) {
    gchar *target = g_canonicalize_filename(path, NULL);
    for (int depth = 0; depth < 40; depth++) {
        gchar *physical = realpath(target, NULL);
        if (physical) { g_free(target); return physical; }
        /* realpath needs an existing final file. Follow dangling links too so
         * a later target creation is noticed without polling. */
        gchar *link = g_file_read_link(target, NULL);
        if (!link) break;
        gchar *dir = g_path_get_dirname(target);
        gchar *next = g_canonicalize_filename(link, dir);
        g_free(dir); g_free(link); g_free(target);
        target = next;
    }
    return target;
}

static gboolean arm(ConfigWatch *watch, GError **error) {
    if (watch->monitor) {
        g_file_monitor_cancel(watch->monitor);
        g_clear_object(&watch->monitor);
    }
    if (watch->target_monitor) g_file_monitor_cancel(watch->target_monitor);
    g_clear_object(&watch->target_monitor);
    g_clear_object(&watch->target);
    if (g_file_query_file_type(watch->directory, G_FILE_QUERY_INFO_NONE, NULL) != G_FILE_TYPE_DIRECTORY)
        return TRUE; /* parent monitor detects a later creation */
    watch->monitor = g_file_monitor_directory(watch->directory, G_FILE_MONITOR_WATCH_MOVES, NULL, error);
    if (!watch->monitor) return FALSE;
    g_signal_connect(watch->monitor, "changed", G_CALLBACK(changed), watch);
    gchar *resolved = resolve_target(watch->path);
    watch->target = g_file_new_for_path(resolved);
    g_free(resolved);
    if (!g_file_equal(watch->target, watch->file)) {
        GFile *dir = g_file_get_parent(watch->target);
        while (dir && g_file_query_file_type(dir, G_FILE_QUERY_INFO_NONE, NULL) != G_FILE_TYPE_DIRECTORY) {
            GFile *parent = g_file_get_parent(dir);
            g_object_unref(dir);
            dir = parent;
        }
        if (dir) {
            watch->target_monitor = g_file_monitor_directory(dir, G_FILE_MONITOR_WATCH_MOVES, NULL, error);
            g_object_unref(dir);
            if (!watch->target_monitor) return FALSE;
            g_signal_connect(watch->target_monitor, "changed", G_CALLBACK(changed), watch);
        }
    }
    return TRUE;
}

static gboolean reload(gpointer data) {
    ConfigWatch *watch = data;
    watch->reload_source = 0;
    GError *error = NULL;
    if (watch->rearm) {
        watch->rearm = FALSE;
        arm(watch, &error);
    }
    watch->changed(watch->path, error, watch->data);
    g_clear_error(&error);
    return G_SOURCE_REMOVE;
}

static void changed(GFileMonitor *monitor, GFile *file, GFile *other,
                    GFileMonitorEvent event, gpointer data) {
    (void)event;
    ConfigWatch *watch = data;
    GFile *target = monitor == watch->parent ? watch->directory
        : monitor == watch->target_monitor ? watch->target : watch->file;
    gboolean directory_event = (file && g_file_equal(file, watch->directory))
        || (monitor == watch->target_monitor && ((file && g_file_has_prefix(target, file))
                                                || (other && g_file_has_prefix(target, other))));
    if (!(file && g_file_equal(file, target)) && !(other && g_file_equal(other, target)) && !directory_event) return;
    watch->rearm = TRUE;
    if (watch->reload_source) g_source_remove(watch->reload_source);
    watch->reload_source = g_timeout_add(100, reload, watch);
}

ConfigWatch *config_watch_new(const char *path, ConfigChanged callback, gpointer data, GError **error) {
    ConfigWatch *watch = g_new0(ConfigWatch, 1);
    watch->path = g_strdup(path);
    watch->changed = callback;
    watch->data = data;
    gchar *dir = g_path_get_dirname(path);
    if (g_mkdir_with_parents(dir, 0700) != 0) {
        g_set_error(error, G_FILE_ERROR, g_file_error_from_errno(errno),
                    "create config directory: %s", g_strerror(errno));
        g_free(dir);
        config_watch_free(watch);
        return NULL;
    }
    watch->directory = g_file_new_for_path(dir);
    watch->file = g_file_new_for_path(path);
    g_free(dir);
    GFile *parent = g_file_get_parent(watch->directory);
    watch->parent = g_file_monitor_directory(parent, G_FILE_MONITOR_WATCH_MOVES, NULL, error);
    g_object_unref(parent);
    if (!watch->parent) { config_watch_free(watch); return NULL; }
    g_signal_connect(watch->parent, "changed", G_CALLBACK(changed), watch);
    if (!arm(watch, error)) { config_watch_free(watch); return NULL; }
    return watch;
}

void config_watch_free(ConfigWatch *watch) {
    if (!watch) return;
    if (watch->reload_source) g_source_remove(watch->reload_source);
    if (watch->monitor) g_file_monitor_cancel(watch->monitor);
    if (watch->parent) g_file_monitor_cancel(watch->parent);
    if (watch->target_monitor) g_file_monitor_cancel(watch->target_monitor);
    g_clear_object(&watch->target_monitor);
    g_clear_object(&watch->target);
    g_clear_object(&watch->monitor);
    g_clear_object(&watch->parent);
    g_clear_object(&watch->file);
    g_clear_object(&watch->directory);
    g_free(watch->path);
    g_free(watch);
}
