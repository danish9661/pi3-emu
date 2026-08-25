/*
 * pi3-ctl N4 bridge exerciser (C) for pi3-emu.
 *
 * Build in the guest with the bundled tcc:
 *     tcc -o bridge-demo bridge-demo.c && ./bridge-demo
 *
 * Browser -> guest: watches INPUT pin 23. Click a UI Bridge button (23/24/25/
 * 26/27) to drive it; the guest reads the level via GPLEV and prints it.
 * Guest -> browser: echoes that level out to OUTPUT pin 21, so the UI toolbar
 * shows "guest->browser: G21=1/0" -> full round-trip verification.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define IN_PIN  23
#define OUT_PIN 21

static int read_val(int p)
{
    char path[64], buf[8];
    FILE *f = fopen(path, "r");
    if (!f) return -1;
    if (!fgets(buf, sizeof(buf), f)) { fclose(f); return -1; }
    fclose(f);
    return atoi(buf);
}

static void write_val(int p, int v)
{
    char path[64];
    FILE *f = fopen(path, "w");
    if (f) { fprintf(f, "%d", v); fclose(f); }
}

static void export_pin(int p, const char *dir)
{
    char path[64];
    snprintf(path, sizeof(path), "/sys/class/gpio/gpio%d/direction", p);
    if (access(path, F_OK) != 0) {
        FILE *e = fopen("/sys/class/gpio/export", "w");
        if (e) { fprintf(e, "%d", p); fclose(e); }
        usleep(200000);
    }
    FILE *d = fopen(path, "w");
    if (d) { fprintf(d, "%s", dir); fclose(d); }
}

int main(void)
{
    if (access("/sys/class/gpio", F_OK) != 0) {
        fprintf(stderr, "ERROR: /sys/class/gpio absent (kernel lacks CONFIG_GPIO_SYSFS?)\n");
        return 1;
    }
    printf("pi3-ctl bridge demo (C): watching GPIO%d (in), echoing to GPIO%d (out)\n",
           IN_PIN, OUT_PIN);
    printf("  UI Bridge buttons drive the input; watch 'guest->browser' for G%d.\n", OUT_PIN);
    export_pin(IN_PIN, "in");
    export_pin(OUT_PIN, "out");
    while (1) {
        int v = read_val(IN_PIN);
        printf("GPIO%d (browser->guest) = %d\n", IN_PIN, v);
        write_val(OUT_PIN, v > 0 ? 1 : 0);
        fflush(stdout);
        sleep(1);
    }
    return 0;
}
