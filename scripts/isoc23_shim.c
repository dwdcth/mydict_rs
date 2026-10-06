/* glibc 兼容垫片：pyke 预编译 onnxruntime 按新版 glibc 构建，引用 C23 的
 * __isoc23_* 符号（glibc>=2.38）。转发给普通 strtol 族即可——差异仅在
 * base=0 时 "0b" 前缀的解析，词典服务用不到该路径。 */
#define _GNU_SOURCE
#include <stdlib.h>

long __isoc23_strtol(const char *n, char **e, int b) { return strtol(n, e, b); }
long long __isoc23_strtoll(const char *n, char **e, int b) { return strtoll(n, e, b); }
unsigned long __isoc23_strtoul(const char *n, char **e, int b) { return strtoul(n, e, b); }
unsigned long long __isoc23_strtoull(const char *n, char **e, int b) { return strtoull(n, e, b); }

long __isoc23_strtoll_l(const char *n, char **e, int b, locale_t l) {
    return strtoll_l(n, e, b, l);
}
unsigned long long __isoc23_strtoull_l(const char *n, char **e, int b, locale_t l) {
    return strtoull_l(n, e, b, l);
}
