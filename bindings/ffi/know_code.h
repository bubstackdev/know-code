#include <stdint.h>
uint32_t kc_abi_version(void);
/* UTF-8 inputs. One owned UTF-8 JSON result; release using kc_string_free. */
char *kc_query_json(const char *database, const char *request);
/* Workspace registration/status/refresh/wait; same ownership as query results. */
char *kc_workspace_json(const char *request);
void kc_string_free(char *value);
