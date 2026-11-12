#ifndef KNOW_CODE_H
#define KNOW_CODE_H

#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
uint32_t kc_abi_version(void);
/* Shared fail-fast admission and compact status; compare the returned version. */
uint32_t kc_query_contract_version(void);
/* NUL-terminated inputs checked for UTF-8. Query requests are scanned only up
 * to kc_query_max_request_bytes()+1 after admission. One owned UTF-8 JSON
 * result; release using kc_string_free. NULL inputs return invalid_request. */
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
/* Synchronous early reservation: NULL scope selects the account/default scope.
 * Success returns NULL and sets *out; failure sets *out=NULL and returns owned
 * error JSON. Keep the handle through host parsing; submit exactly once. */
typedef struct QueryReservation KcQueryReservation;
uint32_t kc_query_max_request_bytes(void);
char *kc_query_reserve(const char *scope, KcQueryReservation **out);
void kc_query_reservation_free(KcQueryReservation *handle);
char *kc_query_json_reserved(const char *database, const char *request,
                             uint64_t elapsed_ms, const KcQueryCancel *cancellation,
                             const KcQueryReservation *reservation);
/* Workspace registration/status/refresh/wait; same ownership as query results. */
char *kc_workspace_json(const char *request);
void kc_string_free(char *value);

#ifdef __cplusplus
}
#endif
#endif /* KNOW_CODE_H */
