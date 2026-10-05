/* Exact packed-header data buffers the guest arms send (recorded with the libva shim, ticket 08)
 * plus a decode of the SPS fields with mesa's own vl_rbsp.h (src/util/vl_rbsp.h, mesa 26.1.8),
 * following the field order of parseEncSpsParamsH264 (src/gallium/frontends/va/picture_h264_enc.c).
 *
 * Result:
 *   failing (High, CQP, default GOP => stalls): profile=100 level=30 chroma=1 poc_type=2
 *                                               max_num_ref=1 640x368 frame_mbs_only=1
 *   g1      (High, all-intra     => works)    : profile=100 level=30 chroma=1 poc_type=2
 *                                               max_num_ref=0 640x368 frame_mbs_only=1
 *   main    (Main, CQP           => works)    : profile= 77 level=30 poc_type=2
 *                                               max_num_ref=1 640x368 frame_mbs_only=1
 * The SPS fields alone do not separate the stalling arm from the working ones (main also has
 * max_num_ref=1), so the trigger is not a single decoded field.
 *
 * A faithful copy of the handler's outer scan loop - including the `vl_rbsp_init(&rbsp, &vlc, ...)`
 * call that advances the outer `vlc`, which the first skeleton omitted - terminates for all three
 * buffers at both emulation-byte settings (2 iterations with emulation bytes on, 1 with them off,
 * bits_left reaching 0). So the spin is NOT in the outer scan loop; it must be inside the parse
 * helpers it calls (parseEncSpsParamsH264 / parseEncPpsParamsH264 / vlVaAddRawHeader) or in
 * vl_rbsp_init's emulation-byte handling for the real context configuration.
 *
 * Build: nix develop path:$PWD --command gcc -O2 -o harness 08-cqp-buffer-bytes.c -I.  (with
 * util/vl_vlc.h and util/vl_rbsp.h copied from mesa, and a one-line util/u_math.h shim).
 */
#include <assert.h>
#include <stdio.h>
#include <stdint.h>
#include <stdbool.h>
#include "util/vl_rbsp.h"

static void dump(const char *what, const uint8_t *buf, unsigned size)
{
   struct vl_vlc vlc = {0};
   const void *inputs[1] = { buf };
   unsigned sizes[1] = { size };
   struct vl_rbsp rbsp;

   vl_vlc_init(&vlc, 1, inputs, sizes);
   /* the handler's scan: find 0x000001 within the first 64 bytes, then eat 24 bits */
   for (int i = 0; i < 64 && vl_vlc_bits_left(&vlc) >= 24; ++i) {
      if (vl_vlc_peekbits(&vlc, 24) == 0x000001)
         break;
      vl_vlc_eatbits(&vlc, 8);
      vl_vlc_fillbits(&vlc);
   }
   vl_vlc_eatbits(&vlc, 24);
   if (vl_vlc_valid_bits(&vlc) < 15) vl_vlc_fillbits(&vlc);
   vl_vlc_eatbits(&vlc, 1);
   (void)vl_vlc_get_uimsbf(&vlc, 2);   /* nal_ref_idc */
   unsigned nut = vl_vlc_get_uimsbf(&vlc, 5);
   vl_rbsp_init(&rbsp, &vlc, ~0, 0 /* emulation bytes off: field values are what matter */);

   unsigned profile = vl_rbsp_u(&rbsp, 8);
   vl_rbsp_u(&rbsp, 6); vl_rbsp_u(&rbsp, 2);
   unsigned level = vl_rbsp_u(&rbsp, 8);
   unsigned sps_id = vl_rbsp_ue(&rbsp);
   unsigned chroma = 99;
   if (profile == 100 || profile == 110 || profile == 122 || profile == 244 || profile == 44 ||
       profile == 83 || profile == 86 || profile == 118 || profile == 128 || profile == 138 ||
       profile == 139 || profile == 134 || profile == 135) {
      chroma = vl_rbsp_ue(&rbsp);
      if (chroma == 3) vl_rbsp_u(&rbsp, 1);
      vl_rbsp_ue(&rbsp); vl_rbsp_ue(&rbsp); vl_rbsp_u(&rbsp, 1);
      if (vl_rbsp_u(&rbsp, 1)) { printf("%-8s SPS scaling matrix present\n", what); return; }
   }
   unsigned log2_max_frame_num_minus4 = vl_rbsp_ue(&rbsp);
   unsigned poc_type = vl_rbsp_ue(&rbsp);
   int64_t num_ref_cycle = -1;
   if (poc_type == 0) vl_rbsp_ue(&rbsp);
   else if (poc_type == 1) {
      vl_rbsp_u(&rbsp, 1);
      vl_rbsp_se(&rbsp); vl_rbsp_se(&rbsp);
      num_ref_cycle = vl_rbsp_ue(&rbsp);
   }
   unsigned max_num_ref = vl_rbsp_ue(&rbsp);
   vl_rbsp_u(&rbsp, 1);
   unsigned w_mbs = vl_rbsp_ue(&rbsp) + 1;
   unsigned h_mu = vl_rbsp_ue(&rbsp) + 1;
   unsigned frame_mbs_only = vl_rbsp_u(&rbsp, 1);

   printf("%-8s nut=%u profile=%u level=%u sps_id=%u chroma=%u poc_type=%u "
          "num_ref_cycle=%lld max_num_ref=%u %ux%u frame_mbs_only=%u (bits_left=%u valid=%u)\n",
          what, nut, profile, level, sps_id, chroma, poc_type, (long long)num_ref_cycle,
          max_num_ref, w_mbs * 16, h_mu * 16 * (frame_mbs_only ? 1 : 2), frame_mbs_only,
          vl_vlc_bits_left(&rbsp.nal), vl_vlc_valid_bits(&rbsp.nal));
}

static const uint8_t buf_failing[40] = { 0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x0c, 0x1e, 0xac, 0x2b, 0x40, 0x50, 0x17, 0xfc, 0xb8, 0x0b, 0x50, 0x10, 0x10, 0x14, 0x00, 0x00, 0x0f, 0xa0, 0x00, 0x03, 0xa6, 0xca, 0x3c, 0x20, 0x10, 0xa8, 0x00, 0x00, 0x00, 0x01, 0x68, 0xee, 0x38, 0xb0 };
static const uint8_t buf_g1[40] = { 0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x1c, 0x1e, 0xac, 0x2b, 0x81, 0x40, 0x5f, 0xf2, 0xe0, 0x2d, 0x40, 0x40, 0x40, 0x50, 0x00, 0x00, 0x3e, 0x80, 0x00, 0x0e, 0x9b, 0x28, 0xf0, 0x80, 0x42, 0xa0, 0x00, 0x00, 0x00, 0x01, 0x68, 0xee, 0x38, 0xb0 };
static const uint8_t buf_main[39] = { 0x00, 0x00, 0x00, 0x01, 0x67, 0x4d, 0x4c, 0x1e, 0x95, 0xa0, 0x28, 0x0b, 0xfe, 0x5c, 0x05, 0xa8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00, 0x01, 0xd3, 0x65, 0x1e, 0x10, 0x08, 0x54, 0x00, 0x00, 0x00, 0x01, 0x68, 0xee, 0x38, 0x80 };

int main(void)
{
   dump("failing", buf_failing, sizeof(buf_failing));
   dump("g1", buf_g1, sizeof(buf_g1));
   dump("main", buf_main, sizeof(buf_main));
   return 0;
}
