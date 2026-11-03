#pragma once

#ifndef G_LOG_DOMAIN
#define G_LOG_DOMAIN "omapower"
#endif
#include <gio/gio.h>

/* Configuration loading, editing and monitoring. */
typedef struct ConfigWatch ConfigWatch;
typedef void (*ConfigChanged)(const char *path, const GError *error, gpointer data);

GHashTable *load_commands(const char *path, GError **error);
int edit_command(const char *action, const char *name);
ConfigWatch *config_watch_new(const char *path, ConfigChanged changed, gpointer data, GError **error);
void config_watch_free(ConfigWatch *watch);

/* Process monitoring. */
typedef struct ProcessWatch ProcessWatch;
typedef void (*ProcessWatchChanged)(ProcessWatch *watch, const GError *error, gpointer data);

ProcessWatch *process_watch_new(GHashTable *commands, gboolean force_poll,
                              ProcessWatchChanged changed, gpointer data, GError **error);
gboolean process_watch_set_commands(ProcessWatch *watch, GHashTable *commands, GError **error);
GHashTable *process_watch_active(ProcessWatch *watch); /* caller owns the returned set */
gboolean process_watch_running(ProcessWatch *watch);
const char *process_watch_mode(ProcessWatch *watch);
void process_watch_free(ProcessWatch *watch);

/* Stream service state and configuration as newline-delimited JSON. */
int watch_widget(void);

static inline void remove_source(guint *source) {
    if (*source) g_source_remove(*source);
    *source = 0;
}
