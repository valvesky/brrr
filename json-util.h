#ifndef OMAPOWER_JSON_UTIL_H
#define OMAPOWER_JSON_UTIL_H

#include <glib.h>
#include <stdlib.h>
#include <string.h>
#include "vendor/json.h"

/* Parsed documents and encoded strings from json.h are single malloc blocks. */
static inline struct json_value_s *json_member(struct json_value_s *root, const char *name) {
    struct json_object_s *object = json_value_as_object(root);
    struct json_value_s *found = NULL;
    if (!object) return NULL;
    for (struct json_object_element_s *p = object->start; p; p = p->next)
        if (p->name->string_size == strlen(name) && !memcmp(p->name->string, name, strlen(name)))
            found = p->value; /* Match JSON-GLib's last-member-wins behavior. */
    return found;
}

static inline char *json_quote(const char *text) {
    struct json_string_s string = { text, strlen(text) };
    struct json_value_s value = { &string, json_type_string };
    return json_write_minified(&value, NULL);
}

/* Command names are always encoded through the library, never interpolated. */
static inline gchar *json_names(GHashTable *names) {
    GString *output = g_string_new("[");
    GList *keys = names ? g_list_sort(g_hash_table_get_keys(names), (GCompareFunc)g_strcmp0) : NULL;
    for (GList *p = keys; p; p = p->next) {
        char *quoted = json_quote(p->data);
        if (!quoted) g_error("cannot encode JSON command name");
        if (p != keys) g_string_append_c(output, ',');
        g_string_append(output, quoted);
        free(quoted);
    }
    g_list_free(keys);
    g_string_append_c(output, ']');
    return g_string_free(output, FALSE);
}

#endif
