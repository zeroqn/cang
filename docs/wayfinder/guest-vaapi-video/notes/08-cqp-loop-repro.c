/* Host-side skeleton of the loop in mesa's vlVaHandleVAEncPackedHeaderDataBufferTypeH264
 * (src/gallium/frontends/va/picture_h264_enc.c, mesa 26.1.8), run over the exact packed-header
 * data buffers the guest arms send (recorded with the libva shim in ticket 08).
 *
 * What it showed, and what it did NOT:
 *  - with asserts on, the failing High-profile CQP buffer trips
 *    `vl_vlc_peekbits: Assertion 'vl_vlc_valid_bits(vlc) >= num_bits || vlc->data >= vlc->end'
 *    failed` (src/util/vl_vlc.h:227), i.e. the loop reaches a peek with fewer valid bits than
 *    requested while data < end;
 *  - with -DNDEBUG it then spins past 1e6 iterations with `bits_left = 3750968912`
 *    (= (unsigned)(32 - invalid_bits) after eatbits ran past the buffer end) - but it does so for
 *    ALL THREE inputs, including the two arms that work in the VM.
 * So this skeleton is NOT faithful enough to prove the infinite loop: it omits
 * `vl_rbsp_init(&rbsp, &vlc, ...)` and the parse calls that follow, which advance the outer `vlc`
 * between iterations. A faithful reproduction has to include them before this becomes a root cause.
 *
 * Build (needs a C compiler; the repo devshell has one):
 *   nix develop path:$PWD --command gcc -O2 -o vlc_repro vlc_repro.c -I.
 * with util/vl_vlc.h copied from mesa and a minimal util/u_math.h shim.
 */
/* Minimal reproduction of the loop in mesa's vlVaHandleVAEncPackedHeaderDataBufferTypeH264
 * (src/gallium/frontends/va/picture_h264_enc.c), using mesa's own vl_vlc.h, on the exact
 * packed-header data buffer the stalling guest arm sends. No VM needed. */
#include <stdio.h>
#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#include <assert.h>

/* mesa's vl_vlc.h needs util/u_math.h for util_logbase2 and UTIL_ARCH_BIG_ENDIAN only in the
 * table-compression helpers, which we do not use; provide the little we need. */
#include "vl_vlc.h"

static unsigned long iterations;

/* stand-ins for the parsing helpers the real handler calls */
static void parse_sps(void) {}
static void parse_pps(void) {}
static void add_raw_header(void) {}

static void run(const char *what, const uint8_t *data, unsigned size)
{
   struct vl_vlc vlc = {0};
   const void *inputs[1] = { data };
   unsigned sizes[1] = { size };
   int nal_start = -1;
   unsigned nal_unit_type = 0, emulation_bytes_start = 0;
   bool is_slice = false;
   unsigned packed_header_emulation_bytes = 0;   /* as the guest's context had it */

   iterations = 0;
   vl_vlc_init(&vlc, 1, inputs, sizes);

   while (vl_vlc_bits_left(&vlc) > 0) {
      if (++iterations > 1000000) {
         printf("%-10s SPINS: >1e6 iterations, bits_left=%u valid_bits=%u data_off=%td\n",
                what, vl_vlc_bits_left(&vlc), vl_vlc_valid_bits(&vlc), vlc.data - data);
         return;
      }
      for (int i = 0; i < 64 && vl_vlc_bits_left(&vlc) >= 24; ++i) {
         if (vl_vlc_peekbits(&vlc, 24) == 0x000001)
            break;
         vl_vlc_eatbits(&vlc, 8);
         vl_vlc_fillbits(&vlc);
      }

      unsigned start = vlc.data - data - vl_vlc_valid_bits(&vlc) / 8;
      emulation_bytes_start = 4;
      if (start > 0 && data[start - 1] == 0x00) {
         start--;
         emulation_bytes_start++;
      }
      if (nal_start >= 0)
         add_raw_header();
      nal_start = start;
      is_slice = false;

      vl_vlc_eatbits(&vlc, 24);
      if (vl_vlc_valid_bits(&vlc) < 15)
         vl_vlc_fillbits(&vlc);
      vl_vlc_eatbits(&vlc, 1);
      unsigned nal_ref_idc = vl_vlc_get_uimsbf(&vlc, 2);
      nal_unit_type = vl_vlc_get_uimsbf(&vlc, 5);
      (void)nal_ref_idc;

      if (nal_unit_type == 5 || nal_unit_type == 1) { is_slice = true; }
      else if (nal_unit_type == 7) parse_sps();
      else if (nal_unit_type == 8) parse_pps();

      if (!packed_header_emulation_bytes)
         break;
   }
   printf("%-10s ok: %lu iterations, bits_left=%u\n", what, iterations, vl_vlc_bits_left(&vlc));
}

static const uint8_t sps_failing[40] = {
   0x00,0x00,0x00,0x01,0x67,0x64,0x0c,0x1e,0xac,0x2b,0x40,0x50,0x17,0xfc,0xb8,0x0b,
   0x50,0x10,0x10,0x14,0x00,0x00,0xfa,0x00,0x03,0x6c,0xa3,0xc2,0x01,0x0a,0x80,0x00,
   0x00,0x01,0x68,0xee,0x38,0xb0,0x00,0x00};

static const uint8_t sps_g1[40] = {
   0x00,0x00,0x00,0x01,0x67,0x64,0x1c,0x1e,0xac,0x2b,0x81,0x40,0x5f,0xf2,0xe0,0x2d,
   0x40,0x40,0x40,0x50,0x00,0x00,0x3e,0x80,0x00,0x0e,0x9b,0x28,0xf0,0x80,0x42,0xa0,
   0x00,0x00,0x00,0x01,0x68,0xee,0x38,0xb0};

static const uint8_t sps_main[39] = {
   0x00,0x00,0x00,0x01,0x67,0x4d,0x4c,0x1e,0x95,0xa0,0x28,0x0b,0xfe,0x5c,0x05,0xa8,
   0x08,0x08,0x0a,0x00,0x00,0x07,0xd0,0x00,0x01,0xd3,0x65,0x1e,0x10,0x08,0x54,0x00,
   0x00,0x00,0x01,0x68,0xee,0x38,0x80};

int main(void)
{
   run("failing", sps_failing, sizeof(sps_failing));
   run("g1", sps_g1, sizeof(sps_g1));
   run("main", sps_main, sizeof(sps_main));
   return 0;
}
