#include <stdint.h>
uint32_t kc_abi_version(void);
/* Query policy 3: deadlines, byte caps, portable cursors and cancellation. */
uint32_t kc_query_contract_version(void);
/* UTF-8 inputs. One owned UTF-8 JSON result; release using kc_string_free. */
char *kc_query_json(const char *database, const char *request);
/* Same ownership; elapsed_ms charges host preparation/async queue time. */
char *kc_query_json_elapsed(const char *database, const char *request, uint64_t elapsed_ms);
/* Each query/callback retains its handle until the native call returns. The
 * caller must not free a handle concurrently with any call using it. Cloned
 * flags inside the native query remain owned independently through cleanup. */
typedef struct QueryCancel KcQueryCancel;
KcQueryCancel *kc_query_cancel_new(void);
void kc_query_cancel(const KcQueryCancel *handle);
void kc_query_cancel_free(KcQueryCancel *handle);
char *kc_query_json_cancellable(const char *database, const char *request,
                               uint64_t elapsed_ms, const KcQueryCancel *handle);
/* Workspace registration/status/refresh/wait; same ownership as query results. */
char *kc_workspace_json(const char *request);
void kc_string_free(char *value);
