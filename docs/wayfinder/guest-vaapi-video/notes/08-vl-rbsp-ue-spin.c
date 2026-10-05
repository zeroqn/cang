/* Host-side reproduction of the guest's CQP encode hang (ticket 08), mesa 26.1.8.
 *
 * `vl_rbsp_ue()` (src/util/vl_rbsp.h) reads exponential-Golomb codes with
 *
 *      while (!vl_vlc_get_uimsbf(&rbsp->nal, 1)) { ++bits; if (bits == 16) vl_rbsp_fillbits(rbsp); }
 *
 * which has no end-of-NAL check: once the packed-header buffer's RBSP is exhausted,
 * vl_vlc_fillbits() cannot add bits, the next bit reads as 0 forever and `bits` increments without
 * bound. `parseEncSpsParamsH264()` calls it for e.g. `seq_parameter_set_id`, so `vaRenderPicture()`
 * never returns - the guest stalls with no syscalls, no EXECBUFFER and an idle host.
 *
 * This program feeds the exact 40-byte packed-header buffer the stalling guest arm sends
 * (SPS+PPS, recorded with the libva shim), consumes it the way the SPS parser does, and then keeps
 * reading. Debug build (-O2): the assertion in vl_vlc_get_uimsbf fires immediately. Release build
 * (-O2 -DNDEBUG, as mesa ships): the call does not return at all, caught by the 3 s alarm.
 *
 *   $ gcc -O2 -DNDEBUG -o ue_spin ue_spin.c -I.     # needs util/vl_vlc.h + util/vl_rbsp.h from mesa
 *   $ ./ue_spin 0
 *     consumed ue #0 = 2 (bits_left=277)
 *     RESULT: vl_rbsp_ue() did NOT return within 3 s after the NAL was exhausted -> infinite loop
 *   $ ./ue_spin 1        # same with emulation-byte handling on
 *     consumed ue #0 = 2 (bits_left=205)
 *     RESULT: ... infinite loop
 *
 * Fix direction: bound the readers, e.g.
 *      while (vl_vlc_bits_left(&rbsp->nal) > 0 && !vl_vlc_get_uimsbf(&rbsp->nal, 1))
 * (or clamp `bits`) in vl_rbsp_ue() / vl_rbsp_se() in src/util/vl_rbsp.h.
 */
#include <stdio.h>
#include <stdint.h>
#include <stdbool.h>
#include <assert.h>
#include <unistd.h>
#include <signal.h>
#include <stdlib.h>
#include "util/vl_rbsp.h"

static const uint8_t buf_failing[40] = {0x00,0x00,0x00,0x01,0x67,0x64,0x0c,0x1e,0xac,0x2b,0x40,0x50,0x17,0xfc,0xb8,0x0b,0x50,0x10,0x10,0x14,0x00,0x00,0xfa,0x00,0x03,0x6c,0xa3,0xc2,0x01,0x0a,0x80,0x00,0x00,0x01,0x68,0xee,0x38,0xb0,0x00,0x00};

static void on_alarm(int sig)
{
   (void)sig;
   printf("RESULT: vl_rbsp_ue() did NOT return within 3 s after the NAL was exhausted -> infinite loop\n");
   fflush(stdout);
   _exit(42);
}

int main(int argc, char **argv)
{
   struct vl_vlc vlc = {0};
   const void *inputs[1] = { buf_failing };
   unsigned sizes[1] = { sizeof(buf_failing) };
   struct vl_rbsp rbsp;
   unsigned emu = argc > 1 ? (unsigned)atoi(argv[1]) : 0;

   vl_vlc_init(&vlc, 1, inputs, sizes);
   for (int i = 0; i < 64 && vl_vlc_bits_left(&vlc) >= 24; ++i) {
      if (vl_vlc_peekbits(&vlc, 24) == 0x000001) break;
      vl_vlc_eatbits(&vlc, 8);
      vl_vlc_fillbits(&vlc);
   }
   vl_vlc_eatbits(&vlc, 24);
   if (vl_vlc_valid_bits(&vlc) < 15) vl_vlc_fillbits(&vlc);
   vl_vlc_eatbits(&vlc, 1);
   (void)vl_vlc_get_uimsbf(&vlc, 2);
   (void)vl_vlc_get_uimsbf(&vlc, 5);
   vl_rbsp_init(&rbsp, &vlc, ~0, emu);

   signal(SIGALRM, on_alarm);
   alarm(3);
   /* consume the RBSP the way the SPS parser does, then keep reading: this is what happens
    * when the parser runs past the end of a packed-header buffer. */
   for (unsigned n = 0; n < 400; n++) {
      unsigned v = vl_rbsp_ue(&rbsp);
      if ((n % 50) == 0)
         printf("  consumed ue #%u = %u (bits_left=%u)\n", n, v, vl_vlc_bits_left(&rbsp.nal));
   }
   alarm(0);
   printf("RESULT: 400 ue() reads returned (no spin) - bits_left now %u\n", vl_vlc_bits_left(&rbsp.nal));
   return 0;
}
