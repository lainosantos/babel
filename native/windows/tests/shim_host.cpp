#include "definitions.h"
#include "BabelTransport.h"
#include <cassert>
#include <array>
#include <thread>

int main() {
    BabelInitializeTransport();
    for (ULONG cable=0;cable<2;++cable) { BabelSetRunning(cable,0,1);BabelSetRunning(cable,1,1); }
    std::array<BYTE,8192> input{},output{};
    for (size_t i=0;i<input.size();++i) input[i]=static_cast<BYTE>(i%251);
    BabelWrite(0,input.data(),static_cast<ULONG>(input.size()));
    BabelRead(1,output.data(),static_cast<ULONG>(output.size()));
    assert((output==std::array<BYTE,8192>{}));
    BabelRead(0,output.data(),static_cast<ULONG>(output.size()));assert(output==input);
    BabelWrite(0,input.data(),static_cast<ULONG>(input.size()));
    BabelSetRunning(0,1,0);BabelSetRunning(0,1,1);
    BabelRead(0,output.data(),static_cast<ULONG>(output.size()));assert((output==std::array<BYTE,8192>{}));
    // Exercise concurrent callbacks and state transitions through actual shim locks.
    std::thread producer([&]{for(int i=0;i<1000;++i)BabelWrite(0,input.data(),192);});
    std::thread consumer([&]{for(int i=0;i<1000;++i)BabelRead(0,output.data(),192);});
    std::thread pause([&]{for(int i=0;i<100;++i){BabelSetRunning(0,0,0);BabelSetRunning(0,0,1);}});
    producer.join();consumer.join();pause.join();
    BabelSetRunning(0,0,0);BabelRead(0,output.data(),static_cast<ULONG>(output.size()));
    assert((output==std::array<BYTE,8192>{}));
}
