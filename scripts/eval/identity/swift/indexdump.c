// Dumps a Swift index store as text, for label.py: every record of every unit that isn't a system unit, once.
//
//     indexdump STORE > dump.tsv
//
//     R <record name> <source path>
//     O <usr> <name> <kind> <subkind> <roles> <line> <col>     an occurrence in that record
//     L <roles> <usr> <name> <kind> <subkind>                  one of that occurrence's relations
//
// Built against the toolchain's libIndexStore with the C API's function-pointer variants (`*_apply_f`), so it needs
// no blocks runtime. The prototypes are declared here, as indexstore.h declares them, so no header is needed.
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef void *indexstore_t;
typedef void *indexstore_error_t;
typedef void *indexstore_unit_reader_t;
typedef void *indexstore_unit_dependency_t;
typedef void *indexstore_record_reader_t;
typedef void *indexstore_occurrence_t;
typedef void *indexstore_symbol_t;
typedef void *indexstore_symbol_relation_t;
typedef struct {
  const char *data;
  size_t length;
} indexstore_string_ref_t;

enum { DEPENDENCY_RECORD = 2 };

extern indexstore_t indexstore_store_create(const char *path, indexstore_error_t *error);
extern const char *indexstore_error_get_description(indexstore_error_t);
extern bool indexstore_store_units_apply_f(indexstore_t, unsigned sorted, void *context,
                                           bool (*applier)(void *context, indexstore_string_ref_t unit_name));
extern indexstore_unit_reader_t indexstore_unit_reader_create(indexstore_t, const char *unit_name,
                                                              indexstore_error_t *error);
extern void indexstore_unit_reader_dispose(indexstore_unit_reader_t);
extern bool indexstore_unit_reader_is_system_unit(indexstore_unit_reader_t);
extern bool indexstore_unit_reader_dependencies_apply_f(indexstore_unit_reader_t, void *context,
                                                        bool (*applier)(void *context, indexstore_unit_dependency_t));
extern int indexstore_unit_dependency_get_kind(indexstore_unit_dependency_t);
extern bool indexstore_unit_dependency_is_system(indexstore_unit_dependency_t);
extern indexstore_string_ref_t indexstore_unit_dependency_get_name(indexstore_unit_dependency_t);
extern indexstore_string_ref_t indexstore_unit_dependency_get_filepath(indexstore_unit_dependency_t);
extern indexstore_record_reader_t indexstore_record_reader_create(indexstore_t, const char *record_name,
                                                                  indexstore_error_t *error);
extern void indexstore_record_reader_dispose(indexstore_record_reader_t);
extern bool indexstore_record_reader_occurrences_apply_f(indexstore_record_reader_t, void *context,
                                                         bool (*applier)(void *context, indexstore_occurrence_t));
extern indexstore_symbol_t indexstore_occurrence_get_symbol(indexstore_occurrence_t);
extern uint64_t indexstore_occurrence_get_roles(indexstore_occurrence_t);
extern void indexstore_occurrence_get_line_col(indexstore_occurrence_t, unsigned *line, unsigned *column);
extern bool indexstore_occurrence_relations_apply_f(indexstore_occurrence_t, void *context,
                                                    bool (*applier)(void *context, indexstore_symbol_relation_t));
extern indexstore_symbol_t indexstore_symbol_relation_get_symbol(indexstore_symbol_relation_t);
extern uint64_t indexstore_symbol_relation_get_roles(indexstore_symbol_relation_t);
extern int indexstore_symbol_get_kind(indexstore_symbol_t);
extern int indexstore_symbol_get_subkind(indexstore_symbol_t);
extern indexstore_string_ref_t indexstore_symbol_get_name(indexstore_symbol_t);
extern indexstore_string_ref_t indexstore_symbol_get_usr(indexstore_symbol_t);

static indexstore_t store;
static char **seen;
static size_t seen_len, seen_cap;

static char *copy(indexstore_string_ref_t s) {
  char *out = malloc(s.length + 1);
  memcpy(out, s.data, s.length);
  out[s.length] = 0;
  return out;
}

// Records are shared between units (a file compiled for two targets): each is dumped once.
static bool first_time(const char *name) {
  for (size_t i = 0; i < seen_len; i++)
    if (strcmp(seen[i], name) == 0) return false;
  if (seen_len == seen_cap) {
    seen_cap = seen_cap ? seen_cap * 2 : 1024;
    seen = realloc(seen, seen_cap * sizeof *seen);
  }
  seen[seen_len++] = strdup(name);
  return true;
}

static void put(indexstore_string_ref_t s) { fwrite(s.data, 1, s.length, stdout); }

static bool relation(void *context, indexstore_symbol_relation_t rel) {
  (void)context;
  indexstore_symbol_t sym = indexstore_symbol_relation_get_symbol(rel);
  printf("L\t%llu\t", (unsigned long long)indexstore_symbol_relation_get_roles(rel));
  put(indexstore_symbol_get_usr(sym));
  putchar('\t');
  put(indexstore_symbol_get_name(sym));
  printf("\t%d\t%d\n", indexstore_symbol_get_kind(sym), indexstore_symbol_get_subkind(sym));
  return true;
}

static bool occurrence(void *context, indexstore_occurrence_t occ) {
  (void)context;
  indexstore_symbol_t sym = indexstore_occurrence_get_symbol(occ);
  unsigned line = 0, col = 0;
  indexstore_occurrence_get_line_col(occ, &line, &col);
  printf("O\t");
  put(indexstore_symbol_get_usr(sym));
  putchar('\t');
  put(indexstore_symbol_get_name(sym));
  printf("\t%d\t%d\t%llu\t%u\t%u\n", indexstore_symbol_get_kind(sym), indexstore_symbol_get_subkind(sym),
         (unsigned long long)indexstore_occurrence_get_roles(occ), line, col);
  indexstore_occurrence_relations_apply_f(occ, NULL, relation);
  return true;
}

static bool dependency(void *context, indexstore_unit_dependency_t dep) {
  (void)context;
  if (indexstore_unit_dependency_get_kind(dep) != DEPENDENCY_RECORD || indexstore_unit_dependency_is_system(dep))
    return true;
  char *name = copy(indexstore_unit_dependency_get_name(dep));
  if (first_time(name)) {
    indexstore_error_t error = NULL;
    indexstore_record_reader_t reader = indexstore_record_reader_create(store, name, &error);
    if (reader) {
      printf("R\t%s\t", name);
      put(indexstore_unit_dependency_get_filepath(dep));
      putchar('\n');
      indexstore_record_reader_occurrences_apply_f(reader, NULL, occurrence);
      indexstore_record_reader_dispose(reader);
    } else {
      fprintf(stderr, "record %s: %s\n", name, indexstore_error_get_description(error));
    }
  }
  free(name);
  return true;
}

static bool unit(void *context, indexstore_string_ref_t unit_name) {
  (void)context;
  char *name = copy(unit_name);
  indexstore_error_t error = NULL;
  indexstore_unit_reader_t reader = indexstore_unit_reader_create(store, name, &error);
  if (reader) {
    if (!indexstore_unit_reader_is_system_unit(reader))
      indexstore_unit_reader_dependencies_apply_f(reader, NULL, dependency);
    indexstore_unit_reader_dispose(reader);
  } else {
    fprintf(stderr, "unit %s: %s\n", name, indexstore_error_get_description(error));
  }
  free(name);
  return true;
}

int main(int argc, char **argv) {
  if (argc != 2) {
    fprintf(stderr, "usage: indexdump STORE\n");
    return 2;
  }
  indexstore_error_t error = NULL;
  store = indexstore_store_create(argv[1], &error);
  if (!store) {
    fprintf(stderr, "%s: %s\n", argv[1], indexstore_error_get_description(error));
    return 1;
  }
  indexstore_store_units_apply_f(store, 1, NULL, unit);
  return 0;
}
