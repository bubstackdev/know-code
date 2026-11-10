#include <stdint.h>
uint32_t kc_abi_version(void);
/* Query policy 1: overall deadlines and complete JSON response byte budgets. */
uint32_t kc_query_contract_version(void);
/* UTF-8 inputs. One owned UTF-8 JSON result; release using kc_string_free. */
char *kc_query_json(const char *database, const char *request);
/* Same ownership; elapsed_ms charges host preparation/async queue time. */
char *kc_query_json_elapsed(const char *database, const char *request, uint64_t elapsed_ms);
/* Workspace registration/status/refresh/wait; same ownership as query results. */
char *kc_workspace_json(const char *request);
void kc_string_free(char *value);
