`timescale 1ns/1ps
// 8-bit successive-approximation register (fixture). A rising `rst` tries the MSB; each
// rising `clk` keeps the bit under test when `cmp` is high (the input is above the DAC),
// clears it otherwise, and tries the next bit. `eoc` rises once the LSB is decided.
module sar(clk, rst, cmp, d, eoc);
  input clk, rst, cmp;
  output [7:0] d;
  output eoc;
  reg [7:0] d, mask;
  reg eoc;
  always @(posedge clk or posedge rst)
    if (rst) begin
      d <= 8'h80;
      mask <= 8'h80;
      eoc <= 1'b0;
    end else if (!eoc) begin
      d <= (cmp ? d : d & ~mask) | (mask >> 1);
      mask <= mask >> 1;
      eoc <= mask[0];
    end
endmodule
