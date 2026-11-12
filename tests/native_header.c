/* Compile as both C and C++: repeated includes and C-linkage consumers. */
#include "../bindings/ffi/know_code.h"
#include "../bindings/ffi/know_code.h"

#ifdef __cplusplus
extern "C" uint32_t kc_abi_version(void);
#endif

uint32_t (*know_code_header_consumer(void))(void) { return kc_abi_version; }
