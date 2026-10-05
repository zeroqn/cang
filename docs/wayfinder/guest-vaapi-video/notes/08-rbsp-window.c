/* Ticket 08 diagnostic (2026-10-06): does mesa's vl_rbsp_init() limit a packed-header RBSP at the
 * NAL boundary when the client says has_emulation_bytes=1?
 *
 * Run against the exact 40-byte packed-header data buffer the stalling guest arm sends (SPS+PPS):
 *
 *   emu=1  RBSP window: data-data=12  end-data=31  bits_left=208  removed=0 escaped=16
 *          profile=100 level=30 sps_id=0  chroma=1 bit_depth_luma_minus8=0 ...
 *   emu=0  RBSP window: data-data=8   end-data=40  bits_left=280  removed=0 escaped=0
 *
 * With emulation-byte handling on (what the client asks for, per its VAEncPackedHeaderParameterBuffer:
 * bit_length 320, has_emulation_bytes 1) the RBSP window ends at byte 31 - the start code of the
 * *following* PPS - and the decoded SPS fields are sane (High profile, level 30, sps_id 0,
 * chroma_format_idc 1). So mesa's NAL-boundary limiting works, the parse does not run past the SPS
 * into the PPS, and "limit the RBSP at the NAL boundary" is NOT the fix. The guest hang is the
 * unbounded vl_rbsp_ue() read alone; the host vaRenderPicture hang that appears once that is bounded
 * is therefore a separate problem, and the likeliest reason the bounds break the working arm is that
 * they also fire during *valid* parses and change legitimate field values.
 */
/* Diagnostic for ticket 08: where does mesa's vl_rbsp_init() put the end of the RBSP for the
 * packed-header data buffer that carries SPS+PPS (40 bytes, has_emulation_bytes=1)? */
#include <stdio.h>
#include <stdint.h>
#include <stdbool.h>
#include <assert.h>
#include "util/vl_rbsp.h"

static const uint8_t buf[40] = {0x00,0x00,0x00,0x01,0x67,0x64,0x0c,0x1e,0xac,0x2b,0x40,0x50,0x17,0xfc,0xb8,0x0b,
                                0x50,0x10,0x10,0x14,0x00,0x00,0xfa,0x00,0x03,0x6c,0xa3,0xc2,0x01,0x0a,0x80,0x00,
                                0x00,0x01,0x68,0xee,0x38,0xb0,0x00,0x00};

int main(int argc, char **argv)
{
   struct vl_vlc vlc = {0};
   const void *inputs[1] = { buf };
   unsigned sizes[1] = { sizeof(buf) };
   struct vl_rbsp rbsp;
   unsigned emu = argc > 1 ? (unsigned)atoi(argv[1]) : 1;

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
   unsigned nut = vl_vlc_get_uimsbf(&vlc, 5);
   printf("nal_unit_type=%u, after header: vlc.data-data=%td\n", nut, vlc.data - buf);

   vl_rbsp_init(&rbsp, &vlc, ~0, emu);
   printf("emu=%u  RBSP window: data-data=%td  end-data=%td  bits_left=%u  removed=%u escaped=%u\n",
          emu, rbsp.nal.data - buf, rbsp.nal.end - buf, vl_vlc_bits_left(&rbsp.nal), rbsp.removed, rbsp.escaped);

   unsigned profile = vl_rbsp_u(&rbsp, 8);
   vl_rbsp_u(&rbsp, 6); vl_rbsp_u(&rbsp, 2);
   unsigned level = vl_rbsp_u(&rbsp, 8);
   unsigned sps_id = vl_rbsp_ue(&rbsp);
   printf("  profile=%u level=%u sps_id=%u  (now at data-data=%td, bits_left=%u)\n",
          profile, level, sps_id, rbsp.nal.data - buf, vl_vlc_bits_left(&rbsp.nal));
   if (profile == 100 || profile == 110 || profile == 122 || profile == 244 || profile == 44 ||
       profile == 83 || profile == 86 || profile == 118 || profile == 128 || profile == 138 ||
       profile == 139 || profile == 134 || profile == 135) {
      unsigned chroma = vl_rbsp_ue(&rbsp);
      if (chroma == 3) vl_rbsp_u(&rbsp, 1);
      unsigned bdl = vl_rbsp_ue(&rbsp), bdc = vl_rbsp_ue(&rbsp);
      vl_rbsp_u(&rbsp, 1);
      unsigned scaling = vl_rbsp_u(&rbsp, 1);
      printf("  chroma=%u bit_depth_luma_minus8=%u bit_depth_chroma_minus8=%u scaling_matrix=%u\n",
             chroma, bdl, bdc, scaling);
   }
   printf("  after the high-profile block: data-data=%td bits_left=%u\n",
          rbsp.nal.data - buf, vl_vlc_bits_left(&rbsp.nal));
   return 0;
}
