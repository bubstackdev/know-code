// Apache-2.0. Narrow declarations of the public IndexStore C ABI.
// Checked against swiftlang/llvm-project next/clang/include/indexstore/indexstore.h.
// This reads compiler records only; storage and graph queries stay in Rust.
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <dlfcn.h>

typedef void *handle;
typedef struct { const char *data; size_t length; } text_ref;
#define API(result, name, args) static result (*name) args
API(unsigned, indexstore_version, (void));
API(handle, indexstore_store_create, (const char *, handle *));
API(void, indexstore_store_dispose, (handle));
API(const char *, indexstore_error_get_description, (handle));
API(void, indexstore_error_dispose, (handle));
API(bool, indexstore_store_units_apply_f, (handle, unsigned, void *, bool (*)(void *, text_ref)));
API(handle, indexstore_unit_reader_create, (handle, const char *, handle *));
API(void, indexstore_unit_reader_dispose, (handle));
API(text_ref, indexstore_unit_reader_get_main_file, (handle));
API(bool, indexstore_unit_reader_is_system_unit, (handle));
API(bool, indexstore_unit_reader_dependencies_apply_f, (handle, void *, bool (*)(void *, handle)));
API(unsigned, indexstore_unit_dependency_get_kind, (handle));
API(text_ref, indexstore_unit_dependency_get_filepath, (handle));
API(text_ref, indexstore_unit_dependency_get_name, (handle));
API(handle, indexstore_record_reader_create, (handle, const char *, handle *));
API(void, indexstore_record_reader_dispose, (handle));
API(bool, indexstore_record_reader_occurrences_apply_f, (handle, void *, bool (*)(void *, handle)));
API(handle, indexstore_occurrence_get_symbol, (handle));
API(uint64_t, indexstore_occurrence_get_roles, (handle));
API(void, indexstore_occurrence_get_line_col, (handle, unsigned *, unsigned *));
API(bool, indexstore_occurrence_relations_apply_f, (handle, void *, bool (*)(void *, handle)));
API(uint64_t, indexstore_symbol_relation_get_roles, (handle));
API(handle, indexstore_symbol_relation_get_symbol, (handle));
API(text_ref, indexstore_symbol_get_name, (handle));
API(text_ref, indexstore_symbol_get_usr, (handle));
API(unsigned, indexstore_symbol_get_kind, (handle));
static handle store;
static bool failed;
static char **records;
static size_t record_count;
static char *wanted_paths;
static size_t wanted_size;
static void error(handle err) {
  if (err) { fprintf(stderr, "%s\n", indexstore_error_get_description(err)); indexstore_error_dispose(err); }
  failed = true;
}
static char *copy(text_ref s) {
  char *p = malloc(s.length + 1);
  if (!p) { fputs("resource-limited: IndexStore reader allocation\n", stderr); exit(1); }
  memcpy(p, s.data, s.length); p[s.length] = 0; return p;
}
static void json(text_ref s) {
  putchar('"');
  for (size_t i = 0; i < s.length; ++i) {
    unsigned char c = (unsigned char)s.data[i];
    if (c == '"' || c == '\\') { putchar('\\'); putchar(c); }
    else if (c < 32) printf("\\u%04x", c);
    else putchar(c);
  }
  putchar('"');
}
static bool relation(void *ctx, handle rel) {
  uint64_t roles = indexstore_symbol_relation_get_roles(rel);
  // A compiler's containment relation supplies context, not a guessed call edge.
  if (roles & ((1ull << 9) | (1ull << 16))) {
    text_ref *enclosing = ctx;
    *enclosing = indexstore_symbol_get_usr(indexstore_symbol_relation_get_symbol(rel));
  }
  return true;
}
static bool occurrence(void *ctx, handle occ) {
  handle symbol = indexstore_occurrence_get_symbol(occ);
  unsigned line, col;
  indexstore_occurrence_get_line_col(occ, &line, &col);
  text_ref enclosing = {"", 0};
  indexstore_occurrence_relations_apply_f(occ, &enclosing, relation);
  printf("{\"path\":"); json(*(text_ref *)ctx);
  printf(",\"usr\":"); json(indexstore_symbol_get_usr(symbol));
  printf(",\"name\":"); json(indexstore_symbol_get_name(symbol));
  printf(",\"enclosing\":"); json(enclosing);
  printf(",\"kind\":%u,\"roles\":%llu,\"line\":%u,\"column\":%u}\n",
         indexstore_symbol_get_kind(symbol),
         (unsigned long long)indexstore_occurrence_get_roles(occ), line, col);
  return !ferror(stdout);
}
static bool wanted(text_ref path) {
  if (!wanted_paths) return true;
  for (size_t offset = 0; offset < wanted_size;) {
    size_t length = strlen(wanted_paths + offset);
    if (length == path.length && memcmp(wanted_paths + offset, path.data, length) == 0) return true;
    offset += length + 1;
  }
  return false;
}
static bool dependency(void *ctx, handle dep) {
  text_ref path = indexstore_unit_dependency_get_filepath(dep);
  if (path.length) { printf("{\"input\":true,\"path\":"); json(path); puts("}"); }
  if (!*(bool *)ctx || indexstore_unit_dependency_get_kind(dep) != 2 || !wanted(path)) return true;
  char *name = copy(indexstore_unit_dependency_get_name(dep));
  for (size_t i = 0; i < record_count; ++i) if (strcmp(records[i], name) == 0) { free(name); return true; }
  char **grown = realloc(records, (record_count + 1) * sizeof(char *));
  if (!grown) { fputs("resource-limited: IndexStore record identities\n", stderr); free(name); failed = true; return false; }
  records = grown; records[record_count++] = name;
  handle err = NULL;
  handle reader = indexstore_record_reader_create(store, name, &err);
  if (!reader) { error(err); return false; }
  printf("{\"record\":true,\"path\":"); json(path); puts("}");
  bool ok = indexstore_record_reader_occurrences_apply_f(reader, &path, occurrence);
  indexstore_record_reader_dispose(reader);
  if (!ok) failed = true;
  return ok;
}
static bool unit(void *ctx, text_ref unit_name) {
  (void)ctx;
  char *name = copy(unit_name);
  handle err = NULL;
  handle reader = indexstore_unit_reader_create(store, name, &err);
  free(name);
  if (!reader) { error(err); return false; }
  if (indexstore_unit_reader_is_system_unit(reader)) { indexstore_unit_reader_dispose(reader); return true; }
  text_ref main_file = indexstore_unit_reader_get_main_file(reader);
  bool indexed = wanted(main_file);
  if (indexed) { printf("{\"unit\":true,\"path\":"); json(main_file); puts("}"); }
  bool ok = indexstore_unit_reader_dependencies_apply_f(reader, &indexed, dependency);
  indexstore_unit_reader_dispose(reader);
  if (!ok) failed = true;
  return ok;
}
#define LOAD(name) do { *(void **)(&name) = dlsym(library, #name); if (!name) { fprintf(stderr, "incompatible Swift libIndexStore: missing %s\n", #name); return 1; } } while (0)
int main(int argc, char **argv) {
  if (argc != 3 && argc != 4) { fputs("usage: indexstore-reader LIBRARY STORE [NUL-SEPARATED-SOURCE-LIST]\n", stderr); return 1; }
  if (argc == 4) {
    FILE *input = fopen(argv[3], "rb");
    if (!input || fseek(input, 0, SEEK_END)) { fputs("cannot read Swift source list\n", stderr); if (input) fclose(input); return 1; }
    long size = ftell(input);
    if (size < 0 || size > 8 * 1024 * 1024 || fseek(input, 0, SEEK_SET)) { fclose(input); fputs("resource-limited: Swift source list exceeds 8 MiB\n", stderr); return 1; }
    wanted_size = (size_t)size;
    wanted_paths = calloc(wanted_size + 1, 1);
    if (!wanted_paths || fread(wanted_paths, 1, wanted_size, input) != wanted_size) { fclose(input); free(wanted_paths); fputs("cannot read Swift source list\n", stderr); return 1; }
    fclose(input);
    if (wanted_size && wanted_paths[wanted_size - 1] != 0) { free(wanted_paths); fputs("invalid Swift source list\n", stderr); return 1; }
  }
  void *library = dlopen(argv[1], RTLD_LOCAL | RTLD_NOW);
  if (!library) { fprintf(stderr, "cannot load Swift libIndexStore: %s\n", dlerror()); return 1; }
  LOAD(indexstore_version);
  if (indexstore_version() / 10000 != 0) { fputs("incompatible Swift libIndexStore ABI major; update Know Code\n", stderr); dlclose(library); return 1; }
  LOAD(indexstore_store_create); LOAD(indexstore_store_dispose);
  LOAD(indexstore_error_get_description); LOAD(indexstore_error_dispose);
  LOAD(indexstore_store_units_apply_f); LOAD(indexstore_unit_reader_create);
  LOAD(indexstore_unit_reader_dispose); LOAD(indexstore_unit_reader_get_main_file); LOAD(indexstore_unit_reader_is_system_unit); LOAD(indexstore_unit_reader_dependencies_apply_f);
  LOAD(indexstore_unit_dependency_get_kind); LOAD(indexstore_unit_dependency_get_filepath);
  LOAD(indexstore_unit_dependency_get_name); LOAD(indexstore_record_reader_create);
  LOAD(indexstore_record_reader_dispose); LOAD(indexstore_record_reader_occurrences_apply_f);
  LOAD(indexstore_occurrence_get_symbol); LOAD(indexstore_occurrence_get_roles);
  LOAD(indexstore_occurrence_get_line_col); LOAD(indexstore_occurrence_relations_apply_f);
  LOAD(indexstore_symbol_relation_get_roles); LOAD(indexstore_symbol_relation_get_symbol);
  LOAD(indexstore_symbol_get_name); LOAD(indexstore_symbol_get_usr); LOAD(indexstore_symbol_get_kind);
  handle err = NULL;
  store = indexstore_store_create(argv[2], &err);
  if (!store) { error(err); dlclose(library); return 1; }
  bool ok = indexstore_store_units_apply_f(store, 1, NULL, unit);
  for (size_t i = 0; i < record_count; ++i) free(records[i]);
  free(records); free(wanted_paths);
  indexstore_store_dispose(store); dlclose(library);
  return !ok || failed || ferror(stdout) ? 1 : 0;
}
