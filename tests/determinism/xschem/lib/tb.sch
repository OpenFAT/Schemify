v {xschem version=3.4.7 file_version=1.2}
N 100 -30 100 -60 {lab=OUT}
N 100 30 100 60 {}
C {inv.sym} 60 0 1 1 {name=X1 WN=2u}
C {res.sym} 100 0 0 0 {name=R1 value='VCC/2' footprint=0805}
C {res.sym} 200 0 2 0 {name=R2 value=1k spice_ignore=short}
C {gnd.sym} 100 60 0 0 {name=l1 lab=GND}
C {lab_pin.sym} 100 -60 0 0 {name=l2 lab=OUT}
C {code.sym} 0 -200 0 0 {name=SIM value=".param VCC=5
.tran 1n 10n"}
T {a note} 0 100 0 0 0.4 0.4 {}
