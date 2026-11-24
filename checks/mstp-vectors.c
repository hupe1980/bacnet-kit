/* Generate deterministic offline interoperability fixtures with bacnet-stack.
 * Run in an empty directory. No serial or network interface is opened. */
#include <stdint.h>
#include <stdio.h>

#include "bacnet/datalink/mstp.h"

static int fixture(const char *name, uint8_t type, uint16_t length)
{
    uint8_t data[1497];
    uint8_t wire[1600];
    for (uint16_t i = 0; i < length; i++) {
        data[i] = (uint8_t)i;
    }
    uint16_t used = MSTP_Create_Frame(
        wire, sizeof(wire), type, 1, 2, data, length);
    if (used == 0) {
        return 1;
    }
    FILE *out = fopen(name, "wb");
    if (!out) {
        return 1;
    }
    size_t written = fwrite(wire, 1, used, out);
    return fclose(out) != 0 || written != used;
}

int main(void)
{
    return fixture("standard-501.bin", 6, 501) ||
        fixture("extended-502.bin", 5, 502) ||
        fixture("extended-1497.bin", 33, 1497);
}
