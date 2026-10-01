.PHONY: all clean

FBS := coprocessor.fbs media.fbs
CPP_OUT := ../coprocessor/include/coprocessor_generated.h ../coprocessor/include/media_generated.h
RUST_OUT := ../portable/crates/coprocessor/src/coprocessor_generated.rs
STAMP := .flatc.stamp

all: $(STAMP)

$(STAMP): $(FBS)
	flatc --cpp -o ../coprocessor/include/ $(FBS)
	flatc --rust --gen-all -o ../portable/crates/coprocessor/src/ coprocessor.fbs
	rustup run nightly rustfmt $(RUST_OUT)
	touch $(STAMP)

clean:
	rm -f $(CPP_OUT) $(RUST_OUT) ../portable/crates/coprocessor/src/media_generated.rs $(STAMP)
